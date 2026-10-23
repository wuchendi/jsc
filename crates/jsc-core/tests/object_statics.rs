//! Integration tests: the `Object` statics `create`, `getPrototypeOf`,
//! `setPrototypeOf`, `getOwnPropertyNames`, `assign`, `hasOwn`, `is`,
//! `fromEntries` and the `Object.prototype` methods `isPrototypeOf`,
//! `propertyIsEnumerable`, `toLocaleString`, `toString` (DESIGN.md §6.3,
//! §7.9). Expected texts are Node's output (strict mode), except jsc's own
//! wording for the nullish `ToObject` `TypeError`, a non-callable
//! `toString`, and the read-only assignment message (no receiver tag).

mod common;
use common::assert_output;

/// Shared helpers: `t(f)` / `tn(f)` run `f` and render its error (name and
/// message / name only).
const PRELUDE: &str =
    "function t(f) { try { f(); return 'ok' } catch (e) { return e.name + ': ' + e.message } }
function tn(f) { try { f(); return 'ok' } catch (e) { return e.name } }
";

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        assert_output(&format!("{PRELUDE}{body}"), expected);
    }
}

/// `Object.create`: the prototype must be an object or `null`; `Properties`
/// goes through `ObjectDefineProperties`.
#[test]
fn create() {
    check(&[
        (
            r"let p = {g: function () { return 'p' }}; let o = Object.create(p); console.log(o.g(), Object.getPrototypeOf(o) === p, Object.keys(o).length, o instanceof Object)",
            "p true 0 true\n",
        ),
        (
            r"let o = Object.create(null); console.log(Object.getPrototypeOf(o), 'toString' in o, typeof o); o.x = 1; console.log(o.x)",
            "null false object\n1\n",
        ),
        (
            r"let o = Object.create({}, {a: {value: 1, enumerable: true}, b: {get: function () { return 2 }}}); console.log(o.a, o.b, Object.keys(o).join(), Object.getOwnPropertyNames(o).join(), t(function () { o.a = 3 }))",
            "1 2 a a,b TypeError: Cannot assign to read only property 'a' of object\n",
        ),
        (
            r"let o = Object.create(Object.prototype, undefined); console.log(Object.getPrototypeOf(o) === Object.prototype)",
            "true\n",
        ),
        (
            r"console.log(t(function () { Object.create(1) }), t(function () { Object.create(undefined) }), t(function () { Object.create('s') }))",
            "TypeError: Object prototype may only be an Object or null: 1 TypeError: Object prototype may only be an Object or null: undefined TypeError: Object prototype may only be an Object or null: s\n",
        ),
        (
            r"console.log(t(function () { Object.create({}, null) }), t(function () { Object.create({}, {a: 1}) }))",
            "TypeError: Cannot convert undefined or null to object TypeError: Property description must be an object: 1\n",
        ),
        (
            r"console.log(Object.create.length, Object.create.name, Object.prototype.propertyIsEnumerable.call(Object, 'create'))",
            "2 create false\n",
        ),
    ]);
}

/// `Object.getPrototypeOf`: `ToObject`, so a primitive reports its realm
/// prototype.
#[test]
fn get_prototype_of() {
    check(&[
        (
            r"class A {} class B extends A {} let b = new B(); console.log(Object.getPrototypeOf(b) === B.prototype, Object.getPrototypeOf(B) === A, Object.getPrototypeOf(Object.prototype))",
            "true true null\n",
        ),
        (
            r"console.log(Object.getPrototypeOf(1) === Number.prototype, Object.getPrototypeOf('s') === String.prototype, Object.getPrototypeOf(true) === Boolean.prototype, Object.getPrototypeOf([]) === Array.prototype, Object.getPrototypeOf(function () {}) === Function.prototype)",
            "true true true true true\n",
        ),
        (
            r"console.log(t(function () { Object.getPrototypeOf(null) }), t(function () { Object.getPrototypeOf() }), Object.getPrototypeOf.length)",
            "TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object 1\n",
        ),
    ]);
}

/// `Object.setPrototypeOf` / `[[SetPrototypeOf]]`: cycles, non-extensible
/// targets, the immutable `%Object.prototype%`, and primitives returned as
/// is.
#[test]
fn set_prototype_of() {
    check(&[
        (
            r"let p = {x: 1}; let o = {}; console.log(Object.setPrototypeOf(o, p) === o, o.x, Object.getPrototypeOf(o) === p); Object.setPrototypeOf(o, null); console.log(o.x, Object.getPrototypeOf(o))",
            "true 1 true\nundefined null\n",
        ),
        (
            r"let a = {}; let b = Object.create(a); console.log(t(function () { Object.setPrototypeOf(a, b) }), t(function () { Object.setPrototypeOf(a, a) }))",
            "TypeError: Cyclic __proto__ value TypeError: Cyclic __proto__ value\n",
        ),
        (
            r"let o = Object.preventExtensions({}); console.log(t(function () { Object.setPrototypeOf(o, {}) }), Object.setPrototypeOf(o, Object.prototype) === o)",
            "TypeError: #<Object> is not extensible true\n",
        ),
        (
            r"console.log(t(function () { Object.setPrototypeOf({}, 1) }), t(function () { Object.setPrototypeOf(null, {}) }), t(function () { Object.setPrototypeOf(undefined, null) }), Object.setPrototypeOf(1, null), Object.setPrototypeOf('s', {}))",
            "TypeError: Object prototype may only be an Object or null: 1 TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object 1 s\n",
        ),
        (
            r"console.log(t(function () { Object.setPrototypeOf(Object.prototype, {}) }), Object.setPrototypeOf(Object.prototype, null) === Object.prototype)",
            "TypeError: Immutable prototype object 'Object.prototype' cannot have their prototype set true\n",
        ),
        (
            r"class A { hi() { return 'A' } } class B { hi() { return 'B' } } let a = new A(); Object.setPrototypeOf(a, B.prototype); console.log(a.hi(), a instanceof B, a instanceof A)",
            "B true false\n",
        ),
        (
            r"let base = {}; Object.setPrototypeOf(Array.prototype, base); base.extra = 7; console.log([].extra); let arr = [1, 2]; arr.push(3); console.log(arr.join())",
            "7\n1,2,3\n",
        ),
        (
            r"console.log(Object.setPrototypeOf.length, Object.setPrototypeOf.name)",
            "2 setPrototypeOf\n",
        ),
    ]);
}

/// `Object.getOwnPropertyNames`: every own string key in own-key order,
/// including the exotic `length` / indices and a function's materialized
/// `length` / `name` / `prototype`.
#[test]
fn get_own_property_names() {
    check(&[
        (
            r"let o = {b: 1, a: 2, 2: 'x', 1: 'y'}; Object.defineProperty(o, 'h', {value: 0}); console.log(Object.getOwnPropertyNames(o).join())",
            "1,2,b,a,h\n",
        ),
        (
            r"let a = [1, , 3]; a.k = 1; console.log(Object.getOwnPropertyNames(a).join(), Object.getOwnPropertyNames([]).join())",
            "0,2,length,k length\n",
        ),
        (
            r"console.log(Object.getOwnPropertyNames('ab').join(), Object.getOwnPropertyNames(new String('xy')).join(), Object.getOwnPropertyNames(5).length)",
            "0,1,length 0,1,length 0\n",
        ),
        (
            r"function f(a, b) {} console.log(Object.getOwnPropertyNames(f).join()); let g = () => 1; console.log(Object.getOwnPropertyNames(g).join())",
            "length,name,prototype\nlength,name\n",
        ),
        (
            r"class C { static s() {} m() {} } console.log(Object.getOwnPropertyNames(C.prototype).join(), Object.getOwnPropertyNames(C).indexOf('s') >= 0, Object.getOwnPropertyNames(C).indexOf('prototype') >= 0)",
            "constructor,m true true\n",
        ),
        (
            r"console.log(t(function () { Object.getOwnPropertyNames(null) }), Object.getOwnPropertyNames.length)",
            "TypeError: Cannot convert undefined or null to object 1\n",
        ),
        (
            r"console.log(Object.getOwnPropertyNames(Object.prototype).indexOf('hasOwnProperty') >= 0, Object.getOwnPropertyNames(Object).indexOf('fromEntries') >= 0)",
            "true true\n",
        ),
    ]);
}

/// `Object.assign`: own enumerable keys in order through `[[Get]]` / strict
/// `[[Set]]` (getters, setters, throwing targets, spread sources).
#[test]
fn assign() {
    check(&[
        (
            r"let t2 = {a: 1}; let r = Object.assign(t2, {b: 2}, null, undefined, {a: 3, c: 4}); console.log(r === t2, Object.keys(t2).join(), t2.a, t2.b, t2.c)",
            "true a,b,c 3 2 4\n",
        ),
        (
            r"let log = []; let src = {}; Object.defineProperty(src, 'g', {get: function () { log.push('get'); return 5 }, enumerable: true}); Object.defineProperty(src, 'h', {value: 6}); let dst = {}; Object.defineProperty(dst, 'g', {set: function (v) { log.push('set ' + v) }}); Object.assign(dst, src); console.log(log.join(), 'h' in dst)",
            "get,set 5 false\n",
        ),
        (
            r"let frozen = Object.freeze({a: 1}); console.log(t(function () { Object.assign(frozen, {a: 2}) }), frozen.a, t(function () { Object.assign(frozen, {}) }))",
            "TypeError: Cannot assign to read only property 'a' of object 1 ok\n",
        ),
        (
            r"console.log(t(function () { Object.assign(null, {}) }), t(function () { Object.assign() }))",
            "TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object\n",
        ),
        (
            r"let s = Object.assign({}, 'ab', [7], 1, true); console.log(Object.keys(s).join(), s[0], s[1]); let n = Object.assign(3, {x: 1}); console.log(typeof n, n.x)",
            "0,1 7 b\nobject 1\n",
        ),
        (
            r"let o = Object.assign({}, {a: 1}, {b: 2}, {c: 3}, {d: 4}, {e: 5}, {f: 6}); console.log(Object.keys(o).join())",
            "a,b,c,d,e,f\n",
        ),
        (
            r"let srcs = [{}, {a: 1}, {b: 2}, {c: 3}, {d: 4}]; let o = Object.assign(...srcs); console.log(Object.keys(o).join(), Object.keys(srcs[0]).join())",
            "a,b,c,d a,b,c,d\n",
        ),
        (
            r"let order = []; let src = {get a() { order.push('a'); delete this.b; return 1 }, b: 2, c: 3}; let d = Object.assign({}, src); console.log(order.join(), Object.keys(d).join())",
            "a a,c\n",
        ),
        (
            r"console.log(Object.assign.length, Object.assign.name)",
            "2 assign\n",
        ),
    ]);
}

/// `Object.hasOwn`: `ToObject(O)` before `ToPropertyKey(P)`.
#[test]
fn has_own() {
    check(&[
        (
            r"let o = {a: 1}; console.log(Object.hasOwn(o, 'a'), Object.hasOwn(o, 'toString'), Object.hasOwn([1], 0), Object.hasOwn([1], 'length'), Object.hasOwn('ab', 1), Object.hasOwn('ab', 2))",
            "true false true true true false\n",
        ),
        (
            r"console.log(t(function () { Object.hasOwn(null, 'a') }), t(function () { Object.hasOwn(undefined, {toString: function () { throw new Error('key') }}) }))",
            "TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object\n",
        ),
        (
            r"let order = []; console.log(Object.hasOwn({x: 1}, {toString: function () { order.push('key'); return 'x' }}), order.join(), Object.hasOwn.length)",
            "true key 2\n",
        ),
    ]);
}

/// `Object.is`: `SameValue`.
#[test]
fn is() {
    check(&[
        (
            r"console.log(Object.is(NaN, NaN), Object.is(0, -0), Object.is(-0, -0), Object.is('a', 'a'), Object.is({}, {}), Object.is(null, undefined), Object.is(1, 1), Object.is())",
            "true false true true false false true true\n",
        ),
        (
            r"let o = {}; console.log(Object.is(o, o), Object.is('ab', 'a' + 'b'), Object.is.length)",
            "true true 2\n",
        ),
    ]);
}

/// `Object.fromEntries` over arrays, `Map` and generators; a non-object
/// entry or a non-iterable throws.
#[test]
fn from_entries() {
    check(&[
        (
            r"let o = Object.fromEntries([['a', 1], ['b', 2], [3, 'x']]); console.log(Object.keys(o).join(), o.a, o.b, o[3])",
            "3,a,b 1 2 x\n",
        ),
        (
            r"let m = new Map(); m.set('k', 1); m.set('j', 2); let o = Object.fromEntries(m); console.log(Object.keys(o).join(), o.k, o.j)",
            "k,j 1 2\n",
        ),
        (
            r"function* g() { yield ['x', 1]; yield ['y', 2] } console.log(JSON.stringify(Object.fromEntries(g())))",
            "{\"x\":1,\"y\":2}\n",
        ),
        (
            r"console.log(t(function () { Object.fromEntries([1]) }), tn(function () { Object.fromEntries(5) }), t(function () { Object.fromEntries(null) }), tn(function () { Object.fromEntries() }))",
            "TypeError: Iterator value 1 is not an entry object TypeError TypeError: Cannot convert undefined or null to object TypeError\n",
        ),
        (
            r"let o = Object.fromEntries([['a', 1], ['a', 2]]); console.log(o.a, Object.getOwnPropertyDescriptor(o, 'a').enumerable, Object.fromEntries.length)",
            "2 true 1\n",
        ),
        (
            r"let k = {toString: function () { return 'key' }}; console.log(Object.keys(Object.fromEntries([[k, 1]])).join(), Object.keys(Object.fromEntries([])).length)",
            "key 0\n",
        ),
    ]);
}

/// `Object.keys` / `values` / `entries` still skip non-enumerable
/// properties and read getters.
#[test]
fn keys_values_entries() {
    check(&[(
        r"let o = {a: 1}; Object.defineProperty(o, 'h', {value: 2}); Object.defineProperty(o, 'g', {get: function () { return 3 }, enumerable: true}); console.log(Object.keys(o).join(), Object.values(o).join(), JSON.stringify(Object.entries(o)))",
        "a,g 1,3 [[\"a\",1],[\"g\",3]]\n",
    )]);
}

/// `Object.values` / `entries` (EnumerableOwnProperties): the key list is
/// taken once, then each key is re-checked when visited, so a getter that
/// deletes a later key or makes it non-enumerable drops it, and a key a
/// getter adds does not appear.
#[test]
fn values_entries_recheck_per_key() {
    check(&[
        (
            r"let o = {get a() { delete this.b; return 1 }, b: 2, c: 3}; console.log(Object.values(o).join()); o = {get a() { delete this.b; return 1 }, b: 2, c: 3}; console.log(JSON.stringify(Object.entries(o)))",
            "1,3\n[[\"a\",1],[\"c\",3]]\n",
        ),
        (
            r"let o = {get a() { Object.defineProperty(this, 'b', {enumerable: false}); return 1 }, b: 2, c: 3}; console.log(Object.values(o).join(), JSON.stringify(Object.entries(o)), Object.keys(o).join())",
            "1,3 [[\"a\",1],[\"c\",3]] a,c\n",
        ),
        (
            r"let o = {get a() { this.z = 9; return 1 }, b: 2}; console.log(Object.values(o).join()); delete o.z; console.log(JSON.stringify(Object.entries(o)))",
            "1,2\n[[\"a\",1],[\"b\",2]]\n",
        ),
        (
            r"let a = [1, 2, 3]; Object.defineProperty(a, 0, {get: function () { Object.defineProperty(a, 2, {enumerable: false}); return 7 }, enumerable: true, configurable: true}); console.log(Object.values(a).join()); Object.defineProperty(a, 2, {enumerable: true}); console.log(JSON.stringify(Object.entries(a)))",
            "7,2\n[[\"0\",7],[\"1\",2]]\n",
        ),
        (
            r"let a = [1, 2, 3, 4]; Object.defineProperty(a, 0, {get: function () { a.length = 2; return 7 }, enumerable: true, configurable: true}); console.log(Object.values(a).join(), JSON.stringify(Object.entries([5, , 6])), Object.values('ab').join())",
            "7,2 [[\"0\",5],[\"2\",6]] a,b\n",
        ),
    ]);
}

/// `Object.prototype.isPrototypeOf`: a non-object argument is `false`
/// before `ToObject(this)`.
#[test]
fn is_prototype_of() {
    check(&[
        (
            r"let p = {}; let o = Object.create(p); console.log(p.isPrototypeOf(o), Object.prototype.isPrototypeOf(o), o.isPrototypeOf(p), p.isPrototypeOf(p))",
            "true true false false\n",
        ),
        (
            r"console.log(Object.prototype.isPrototypeOf(1), Object.prototype.isPrototypeOf.call(null, 1), Array.prototype.isPrototypeOf([]), Function.prototype.isPrototypeOf(Object))",
            "false false true true\n",
        ),
        (
            r"console.log(t(function () { Object.prototype.isPrototypeOf.call(null, {}) }), t(function () { Object.prototype.isPrototypeOf.call(undefined, {}) }), Object.prototype.isPrototypeOf.length)",
            "TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object 1\n",
        ),
        (
            r"class A {} class B extends A {} console.log(A.prototype.isPrototypeOf(new B()), B.prototype.isPrototypeOf(new A()), A.isPrototypeOf(B))",
            "true false true\n",
        ),
    ]);
}

/// `Object.prototype.propertyIsEnumerable`, including exotic array and
/// string keys.
#[test]
fn property_is_enumerable() {
    check(&[
        (
            r"let o = {a: 1}; Object.defineProperty(o, 'h', {value: 1}); console.log(o.propertyIsEnumerable('a'), o.propertyIsEnumerable('h'), o.propertyIsEnumerable('toString'), o.propertyIsEnumerable('zz'))",
            "true false false false\n",
        ),
        (
            r"let a = [1, , 3]; console.log(a.propertyIsEnumerable(0), a.propertyIsEnumerable(1), a.propertyIsEnumerable('length'), a.propertyIsEnumerable(5))",
            "true false false false\n",
        ),
        (
            r"console.log('ab'.propertyIsEnumerable(0), 'ab'.propertyIsEnumerable('length'), Object.prototype.propertyIsEnumerable.call(1, 'x'))",
            "true false false\n",
        ),
        (
            r"function f() {} console.log(f.propertyIsEnumerable('length'), f.propertyIsEnumerable('prototype'), Object.prototype.propertyIsEnumerable.length)",
            "false false 1\n",
        ),
        (
            r"console.log(t(function () { Object.prototype.propertyIsEnumerable.call(null, 'a') }))",
            "TypeError: Cannot convert undefined or null to object\n",
        ),
    ]);
}

/// `Object.prototype.toLocaleString` invokes `this.toString`.
#[test]
fn to_locale_string() {
    check(&[
        (
            r"console.log({}.toLocaleString(), Object.prototype.toLocaleString.call(5), Object.prototype.toLocaleString.call('s'), Object.prototype.toLocaleString.call(true), [1, 2].toLocaleString())",
            "[object Object] 5 s true 1,2\n",
        ),
        (
            r"let o = {toString: function () { return 'custom' }}; console.log(o.toLocaleString(), Object.prototype.toLocaleString.length, Object.prototype.toLocaleString.name)",
            "custom 0 toLocaleString\n",
        ),
        (
            r"console.log(t(function () { Object.prototype.toLocaleString.call(null) }), t(function () { Object.prototype.toLocaleString.call(undefined) }), t(function () { Object.prototype.toLocaleString.call({toString: 1}) }))",
            "TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object TypeError: toString is not a function\n",
        ),
    ]);
}

/// `Object.prototype.toString` builtin tags, also through `call`.
#[test]
fn to_string_tags() {
    check(&[
        (
            r"let ts = Object.prototype.toString; function f() { return ts.call(arguments) } console.log(ts.call([]), ts.call(function () {}), ts.call(new Error('x')), ts.call(new TypeError('x')), ts.call(new Date(0)), f())",
            "[object Array] [object Function] [object Error] [object Error] [object Date] [object Arguments]\n",
        ),
        (
            r"let ts = Object.prototype.toString; console.log(ts.call(true), ts.call(1), ts.call('s'), ts.call(null), ts.call(undefined), ts.call({}), ts.call(new Boolean(false)), ts.call(new Number(1)), ts.call(new String('a')))",
            "[object Boolean] [object Number] [object String] [object Null] [object Undefined] [object Object] [object Boolean] [object Number] [object String]\n",
        ),
        (
            r"console.log(String({}), ({}).toString(), Object.prototype.toString.call(Object.create(null)))",
            "[object Object] [object Object] [object Object]\n",
        ),
    ]);
}

/// Every new builtin has its spec `length` and `name` and is not enumerable.
#[test]
fn realm() {
    check(&[
        (
            r"console.log(['create', 'getPrototypeOf', 'setPrototypeOf', 'getOwnPropertyNames', 'assign', 'hasOwn', 'is', 'fromEntries'].map(function (k) { return k + ':' + Object[k].length + ':' + Object[k].name + ':' + Object.prototype.propertyIsEnumerable.call(Object, k) }).join())",
            "create:2:create:false,getPrototypeOf:1:getPrototypeOf:false,setPrototypeOf:2:setPrototypeOf:false,getOwnPropertyNames:1:getOwnPropertyNames:false,assign:2:assign:false,hasOwn:2:hasOwn:false,is:2:is:false,fromEntries:1:fromEntries:false\n",
        ),
        (
            r"console.log(['isPrototypeOf', 'propertyIsEnumerable', 'toLocaleString'].map(function (k) { let d = Object.getOwnPropertyDescriptor(Object.prototype, k); return k + ':' + Object.prototype[k].length + ':' + Object.prototype[k].name + ':' + d.enumerable + d.writable + d.configurable }).join())",
            "isPrototypeOf:1:isPrototypeOf:falsetruetrue,propertyIsEnumerable:1:propertyIsEnumerable:falsetruetrue,toLocaleString:0:toLocaleString:falsetruetrue\n",
        ),
    ]);
}

/// Arrays in dictionary mode (DESIGN.md §5.4): `getOwnPropertyNames`,
/// `propertyIsEnumerable` and `assign` read each table element's own
/// attributes; `length` stays non-enumerable, also when frozen.
#[test]
fn dictionary_arrays() {
    check(&[
        (
            r"let a = [1, 2, 3]; a.k = 0; Object.defineProperty(a, '1', {enumerable: false}); console.log(Object.getOwnPropertyNames(a).join(), a.propertyIsEnumerable(0), a.propertyIsEnumerable(1), a.propertyIsEnumerable('length'), Object.keys(a).join(), JSON.stringify(Object.assign({}, a)))",
            "0,1,2,length,k true false false 0,2,k {\"0\":1,\"2\":3,\"k\":0}\n",
        ),
        (
            r"let a = Object.freeze([1, , 3]); console.log(Object.getOwnPropertyNames(a).join(), a.propertyIsEnumerable(0), a.propertyIsEnumerable(1), a.propertyIsEnumerable('length'), Object.hasOwn(a, 'length'))",
            "0,2,length true false false true\n",
        ),
        (
            r"let a = [0]; Object.defineProperty(a, '2', {get: function () { return 9 }, enumerable: true}); console.log(Object.getOwnPropertyNames(a).join(), a.propertyIsEnumerable(2), JSON.stringify(Object.assign({}, a)))",
            "0,2,length true {\"0\":0,\"2\":9}\n",
        ),
    ]);
}

/// `Object.prototype.__proto__`: the getter is `ToObject(this)` then its
/// prototype; the setter requires an object-coercible `this`, ignores a
/// non-object non-null value and a primitive `this`, and throws when
/// `[[SetPrototypeOf]]` rejects. It is reached by member writes, computed
/// keys, `Object.assign`, and in programs without any call argument.
#[test]
fn proto_accessor() {
    check(&[
        (
            r"let p = {x: 1}; let o = {}; o.__proto__ = p; console.log(o.x, o.__proto__ === p, Object.getPrototypeOf(o) === p, Object.keys(o).length, o.hasOwnProperty('__proto__')); o.__proto__ = 5; console.log(o.__proto__ === p); o.__proto__ = null; console.log(o.x, Object.getPrototypeOf(o), o.__proto__)",
            "1 true true 0 false\ntrue\nundefined null undefined\n",
        ),
        (
            r"console.log((1).__proto__ === Number.prototype, 's'.__proto__ === String.prototype, [].__proto__ === Array.prototype, (function () {}).__proto__ === Function.prototype); let d = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__'); console.log(typeof d.get, typeof d.set, d.enumerable, d.configurable, d.get.name, d.get.length, d.set.name, d.set.length); console.log(t(function () { d.get.call(undefined) }), t(function () { d.set.call(null, {}) }), d.set.call(1, {}), d.set.call({}), d.get.call(true) === Boolean.prototype)",
            "true true true true\nfunction function false true get __proto__ 0 set __proto__ 1\nTypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object undefined undefined true\n",
        ),
        (
            r"let a = {}; let b = Object.create(a); console.log(t(function () { a.__proto__ = b }), t(function () { Object.prototype.__proto__ = {} }), t(function () { Object.preventExtensions({}).__proto__ = {} })); let o = Object.preventExtensions({}); o.__proto__ = Object.prototype; console.log(Object.prototype.__proto__)",
            "TypeError: Cyclic __proto__ value TypeError: Immutable prototype object 'Object.prototype' cannot have their prototype set TypeError: #<Object> is not extensible\nnull\n",
        ),
        (
            r#"let k = '__pro' + 'to__'; let p = {y: 2}; let o = {}; o[k] = p; console.log(o.y); let q = Object.assign({}, JSON.parse('{"__proto__": {"z": 3}}')); console.log(q.z, Object.keys(q).length)"#,
            "2\n3 0\n",
        ),
    ]);
    // No call passes an argument: the setter still receives its value.
    common::assert_output(
        "let p = {v: 9}; let o = {}; o['__pro' + 'to__'] = p; console.log(o.v)",
        "9\n",
    );
}

/// The legacy accessor methods `__defineGetter__` / `__defineSetter__` /
/// `__lookupGetter__` / `__lookupSetter__` (Annex B.2.2.2-5).
#[test]
fn legacy_accessor_methods() {
    check(&[
        (
            r"let o = {}; let g = function () { return 7 }; let log = []; let s = function (v) { log.push(v) }; console.log(o.__defineGetter__('x', g), o.x); o.__defineSetter__('x', s); o.x = 3; let d = Object.getOwnPropertyDescriptor(o, 'x'); console.log(log.join(), d.get === g, d.set === s, d.enumerable, d.configurable); console.log(o.__lookupGetter__('x') === g, o.__lookupSetter__('x') === s, Object.create(o).__lookupGetter__('x') === g, o.__lookupGetter__('y'), ({y: 1}).__lookupGetter__('y'))",
            "undefined 7\n3 true true true true\ntrue true true undefined undefined\n",
        ),
        (
            r"let o = Object.defineProperty({}, 'x', {value: 1}); console.log(t(function () { o.__defineGetter__('x', function () {}) }), t(function () { ({}).__defineGetter__('x', 1) }), t(function () { Object.prototype.__defineSetter__.call(undefined, 'x', function () {}) }), t(function () { Object.prototype.__lookupGetter__.call(null, 'x') }), tn(function () { Object.preventExtensions({}).__defineGetter__('x', function () {}) }))",
            "TypeError: Cannot redefine property: x TypeError: Object.prototype.__defineGetter__: Expecting function TypeError: Cannot convert undefined or null to object TypeError: Cannot convert undefined or null to object TypeError\n",
        ),
        (
            r"let p = Object.prototype; console.log(p.__defineGetter__.length, p.__defineGetter__.name, p.__defineSetter__.length, p.__lookupGetter__.length, p.__lookupSetter__.name, (1).__lookupGetter__('x'), [1].__lookupGetter__(0))",
            "2 __defineGetter__ 2 1 __lookupSetter__ undefined undefined\n",
        ),
        (
            r"Object.prototype.get = 'g'; Object.prototype.set = 's'; let o = {d: 1}; console.log(o.__lookupGetter__('d'), o.__lookupSetter__('d'), Object.create(o).__lookupGetter__('d'))",
            "undefined undefined undefined\n",
        ),
    ]);
}
