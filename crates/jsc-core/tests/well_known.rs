//! Integration tests: the honoured well-known symbol hooks
//! (DESIGN.md §6.4): `@@toPrimitive` in `ToPrimitive`
//! (hint strings, `GetMethod` errors, the `OrdinaryToPrimitive` fallback,
//! `Date.prototype[@@toPrimitive]`) and `@@toStringTag` in
//! `Object.prototype.toString` (builtin tags as real properties, user tags);
//! `@@hasInstance` in `instanceof` (`InstanceofOperator`) and
//! `Function.prototype[@@hasInstance]`; the String-method dispatch to an
//! object argument's `@@match` / `@@matchAll` / `@@replace` / `@@search` /
//! `@@split`, and `IsRegExp` in `includes` / `startsWith` / `endsWith`.

mod common;
use common::{assert_bool, assert_number, assert_string, assert_uncaught};

// --- @@toPrimitive ---------------------------------------------------------------

#[test]
fn to_primitive_receives_the_hint() {
    let hook = "let hints = []; \
        let o = { [Symbol.toPrimitive](h) { hints.push(h); return 1 } };";
    assert_string(
        &format!("{hook} +o; `${{o}}`; o + ''; o == 1; hints.join()"),
        "number,string,default,default",
    );
    assert_string(
        &format!("{hook} String(o); o < 2; hints.join()"),
        "string,number",
    );
    assert_bool(
        "let o = { [Symbol.toPrimitive]() { return this === o } }; `${o}` === 'true'",
        true,
    );
}

#[test]
fn to_primitive_result_is_used_at_every_conversion_site() {
    let obj = "let o = { [Symbol.toPrimitive]() { return 1 } };";
    assert_number(&format!("{obj} o + 1"), 2.0);
    assert_number(&format!("{obj} +o"), 1.0);
    assert_bool(&format!("{obj} o < 2"), true);
    assert_string(&format!("{obj} `${{o}}`"), "1");
    assert_string(&format!("{obj} String(o)"), "1");
    assert_number(&format!("{obj} ({{ 1: 5 }})[o]"), 5.0);
}

#[test]
fn non_callable_to_primitive_throws() {
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]: 1 }; o + 1",
        "TypeError: Symbol.toPrimitive is not a function",
    );
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]: {} }; `${o}`",
        "TypeError: Symbol.toPrimitive is not a function",
    );
}

#[test]
fn object_result_of_to_primitive_throws() {
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]() { return {} } }; o + 1",
        "TypeError: Cannot convert object to primitive value",
    );
}

#[test]
fn nullish_to_primitive_falls_back_to_ordinary() {
    assert_number(
        "let o = { [Symbol.toPrimitive]: undefined, valueOf() { return 7 } }; o + 1",
        8.0,
    );
    assert_string(
        "let o = { [Symbol.toPrimitive]: null, toString() { return 's' } }; `${o}`",
        "s",
    );
}

#[test]
fn date_to_primitive_default_hint_is_string() {
    assert_bool("new Date(0) + '' === new Date(0).toString()", true);
    assert_string("typeof (new Date(0) + 1)", "string");
    assert_number("+new Date(5)", 5.0);
    assert_number("new Date(5) - 0", 5.0);
    assert_bool(
        "typeof Date.prototype[Symbol.toPrimitive] === 'function' && \
         Date.prototype[Symbol.toPrimitive].name === '[Symbol.toPrimitive]' && \
         Date.prototype[Symbol.toPrimitive].length === 1",
        true,
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Date.prototype, Symbol.toPrimitive); \
         !d.writable && !d.enumerable && d.configurable",
        true,
    );
}

#[test]
fn date_to_primitive_is_generic_and_checks_its_hint() {
    let o = "let o = { toString() { return 's' }, valueOf() { return 1 } };";
    let call = "Date.prototype[Symbol.toPrimitive].call";
    assert_number(&format!("{o} {call}(o, 'number')"), 1.0);
    assert_string(&format!("{o} {call}(o, 'default')"), "s");
    assert_string(&format!("{o} {call}(o, 'string')"), "s");
    assert_uncaught(&format!("{o} {call}(o, 'x')"), "TypeError: Invalid hint");
    assert_uncaught(&format!("{o} {call}(o)"), "TypeError: Invalid hint");
    assert_uncaught(
        &format!("{call}(1, 'number')"),
        "TypeError: Date.prototype [ @@toPrimitive ] called on non-object",
    );
}

// --- @@toStringTag ---------------------------------------------------------------

#[test]
fn builtin_to_string_tags() {
    let tag = |value: &str, expected: &str| {
        assert_string(
            &format!("Object.prototype.toString.call({value})"),
            expected,
        );
    };
    tag("Math", "[object Math]");
    tag("JSON", "[object JSON]");
    tag("new Map()", "[object Map]");
    tag("new Set()", "[object Set]");
    tag("Promise.resolve(1)", "[object Promise]");
    tag("Symbol()", "[object Symbol]");
    tag("Object(Symbol())", "[object Symbol]");
    tag("(function* () {})()", "[object Generator]");
    tag("(async function* () {})()", "[object AsyncGenerator]");
    tag("[].values()", "[object Array Iterator]");
    tag("'ab'[Symbol.iterator]()", "[object String Iterator]");
    tag("new Map().keys()", "[object Map Iterator]");
    tag("new Set().values()", "[object Set Iterator]");
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Map.prototype, Symbol.toStringTag); \
         d.value === 'Map' && !d.writable && !d.enumerable && d.configurable",
        true,
    );
}

#[test]
fn user_to_string_tag_wins() {
    assert_string(
        "let o = { [Symbol.toStringTag]: 'X' }; Object.prototype.toString.call(o)",
        "[object X]",
    );
    assert_string(
        "let o = { [Symbol.toStringTag]: 'X' }; String(o)",
        "[object X]",
    );
    assert_string(
        "class C { get [Symbol.toStringTag]() { return 'C' } } `${new C()}`",
        "[object C]",
    );
    assert_string(
        "class C {} Object.defineProperty(C.prototype, Symbol.toStringTag, { value: 'C' }); \
         `${new C()}`",
        "[object C]",
    );
    assert_string(
        "let a = [1]; a[Symbol.toStringTag] = 'L'; Object.prototype.toString.call(a)",
        "[object L]",
    );
}

#[test]
fn non_string_to_string_tag_is_ignored() {
    assert_string(
        "let o = { [Symbol.toStringTag]: 1 }; Object.prototype.toString.call(o)",
        "[object Object]",
    );
    assert_string(
        "let a = []; a[Symbol.toStringTag] = {}; Object.prototype.toString.call(a)",
        "[object Array]",
    );
    assert_string(
        "delete Map.prototype[Symbol.toStringTag]; Object.prototype.toString.call(new Map())",
        "[object Object]",
    );
}

// --- @@hasInstance ---------------------------------------------------------------

#[test]
fn user_has_instance_on_an_object_rhs() {
    assert_bool("1 instanceof { [Symbol.hasInstance]: () => true }", true);
    assert_bool(
        "({}) instanceof { [Symbol.hasInstance]: () => false }",
        false,
    );
    assert_bool(
        "let seen; let C = { [Symbol.hasInstance](v) { seen = [this, v]; return true } }; \
         let x = {}; x instanceof C && seen[0] === C && seen[1] === x",
        true,
    );
}

#[test]
fn static_has_instance_on_a_class() {
    assert_bool(
        "class Even { static [Symbol.hasInstance](n) { return n % 2 === 0 } } \
         2 instanceof Even && !(3 instanceof Even)",
        true,
    );
    assert_bool(
        "class A { static [Symbol.hasInstance]() { return false } } new A() instanceof A",
        false,
    );
    // A subclass inherits the static hook.
    assert_bool(
        "class A { static [Symbol.hasInstance]() { return true } } class B extends A {} \
         1 instanceof B",
        true,
    );
}

#[test]
fn has_instance_result_is_coerced_with_to_boolean() {
    assert_bool("1 instanceof { [Symbol.hasInstance]: () => 'x' }", true);
    assert_bool("1 instanceof { [Symbol.hasInstance]: () => 0 }", false);
    assert_bool("1 instanceof { [Symbol.hasInstance]: () => ({}) }", true);
    assert_bool("1 instanceof { [Symbol.hasInstance]: () => '' }", false);
    assert_string(
        "typeof (1 instanceof { [Symbol.hasInstance]: () => 1 })",
        "boolean",
    );
}

#[test]
fn non_callable_has_instance_throws() {
    assert_uncaught(
        "({}) instanceof { [Symbol.hasInstance]: 1 }",
        "TypeError: Symbol.hasInstance is not a function",
    );
    assert_uncaught(
        "function F() {} Object.defineProperty(F, Symbol.hasInstance, { value: {} }); \
         ({}) instanceof F",
        "TypeError: Symbol.hasInstance is not a function",
    );
    // An absent hook on a non-callable right-hand side still throws.
    assert_uncaught(
        "({}) instanceof {}",
        "TypeError: Right-hand side of 'instanceof' is not callable",
    );
    // A null hook is absent: `OrdinaryHasInstance` runs.
    assert_bool(
        "function F() {} Object.defineProperty(F, Symbol.hasInstance, { value: null }); \
         new F() instanceof F",
        true,
    );
}

#[test]
fn function_prototype_has_instance_descriptor() {
    let d = "let d = Object.getOwnPropertyDescriptor(Function.prototype, Symbol.hasInstance);";
    assert_bool(
        &format!(
            "{d} typeof d.value === 'function' && !d.writable && !d.enumerable && !d.configurable"
        ),
        true,
    );
    assert_string(&format!("{d} d.value.name"), "[Symbol.hasInstance]");
    assert_number(&format!("{d} d.value.length"), 1.0);
    // Non-writable: an assignment through a constructor does not shadow it.
    assert_bool(
        "function F() {} try { F[Symbol.hasInstance] = () => true } catch (e) {} \
         !Object.getOwnPropertySymbols(F).length && !(1 instanceof F)",
        true,
    );
}

#[test]
fn function_prototype_has_instance_call() {
    let h = "let h = Function.prototype[Symbol.hasInstance];";
    assert_bool(&format!("{h} class A {{}} h.call(A, new A())"), true);
    assert_bool(&format!("{h} class A {{}} h.call(A, {{}})"), false);
    // A non-callable `this` is `false`, not a `TypeError`.
    assert_bool(&format!("{h} h.call({{}}, {{}})"), false);
    // A primitive value is `false`.
    assert_bool(&format!("{h} class A {{}} h.call(A, 1)"), false);
    // `this`'s own `@@hasInstance` is not consulted.
    assert_bool(
        &format!("{h} class A {{ static [Symbol.hasInstance]() {{ return true }} }} h.call(A, 1)"),
        false,
    );
}

#[test]
fn function_prototype_has_instance_on_a_bound_function() {
    let h = "let h = Function.prototype[Symbol.hasInstance];";
    assert_bool(
        &format!("{h} class A {{}} let B = A.bind(null); h.call(B, new A())"),
        true,
    );
    assert_bool(
        &format!("{h} class A {{}} let B = A.bind(null); h.call(B, {{}})"),
        false,
    );
    assert_bool(
        "class A {} let B = A.bind(null).bind(null); new A() instanceof B",
        true,
    );
    // A bound function restarts `InstanceofOperator` on its target.
    assert_bool(
        "class A { static [Symbol.hasInstance]() { return true } } 1 instanceof A.bind(null)",
        true,
    );
}

#[test]
fn builtin_instanceof_arms_unchanged() {
    assert_bool("[] instanceof Array", true);
    assert_bool("({}) instanceof Array", false);
    assert_bool("(() => 1) instanceof Function", true);
    assert_bool("new Date(0) instanceof Date", true);
    assert_bool("[] instanceof Object", true);
    assert_bool("1 instanceof Object", false);
    assert_bool("new TypeError('x') instanceof Error", true);
    assert_bool("new RangeError('x') instanceof TypeError", false);
}

#[test]
fn builtin_instanceof_arms_honour_a_replaced_prototype() {
    assert_bool("Object.setPrototypeOf([], null) instanceof Array", false);
    assert_bool(
        "Object.setPrototypeOf({}, Array.prototype) instanceof Array",
        true,
    );
    assert_bool("Object.create(Array.prototype) instanceof Array", true);
    assert_bool(
        "class X extends Array {} let x = new X(); x instanceof Array && x instanceof X",
        true,
    );
    assert_bool(
        "function f() {} Object.setPrototypeOf(f, null); f instanceof Function",
        false,
    );
    assert_bool("Object.create(Date.prototype) instanceof Date", true);
    assert_bool(
        "Object.setPrototypeOf(new Date(0), null) instanceof Date",
        false,
    );
    assert_bool("[1, 2] instanceof Array", true);
    assert_bool(
        "function f() {} class C {} f instanceof Function && C instanceof Function \
         && f.bind(null) instanceof Function",
        true,
    );
    assert_bool("new Date(0) instanceof Date", true);
    assert_bool("1 instanceof Array || 'a' instanceof Function", false);
}

#[test]
fn instanceof_object_walks_the_prototype_chain() {
    assert_bool("Object.create(null) instanceof Object", false);
    assert_bool("({}) instanceof Object", true);
    assert_bool("class A {} new A() instanceof Object", true);
    assert_bool(
        "class A {} class B extends A {} new B() instanceof Object",
        true,
    );
    assert_bool(
        "let o = {}; Object.setPrototypeOf(o, null); o instanceof Object",
        false,
    );
    assert_bool(
        "let a = []; Object.setPrototypeOf(a, null); a instanceof Object",
        false,
    );
    assert_bool(
        "Object.create(Object.create(null)) instanceof Object",
        false,
    );
    assert_bool("Object.prototype instanceof Object", false);
    assert_bool(
        "(() => 1) instanceof Object && new Map() instanceof Object",
        true,
    );
}

#[test]
fn array_species_getter_keeps_the_builtin_instanceof_arm() {
    // `%Array%` owns `get [Symbol.species]`; only an own `@@hasInstance`
    // leaves the fast arm.
    assert_bool(
        "Object.getOwnPropertySymbols(Array).length > 0 && [] instanceof Array",
        true,
    );
    assert_bool("({}) instanceof Array", false);
    assert_bool(
        "Array[Symbol('x')] = 1; [] instanceof Array && !(1 instanceof Array)",
        true,
    );
    assert_bool(
        "Object.setPrototypeOf(Array, { [Symbol.hasInstance]: () => false }); \
         [] instanceof Array",
        false,
    );
}

#[test]
fn builtin_constructor_own_has_instance_is_honoured() {
    assert_bool(
        "Object.defineProperty(Array, Symbol.hasInstance, { value: () => false }); \
         [] instanceof Array",
        false,
    );
    assert_bool(
        "Object.defineProperty(Error, Symbol.hasInstance, { value: () => true }); \
         1 instanceof TypeError",
        true,
    );
    assert_bool(
        "Object.defineProperty(Object, Symbol.hasInstance, { value: () => true }); \
         1 instanceof Object",
        true,
    );
}

// --- String-method dispatch (@@match / @@matchAll / @@replace / @@search / @@split)

#[test]
fn string_methods_dispatch_to_the_argument_hook() {
    assert_string(
        "'abc'.replace({ [Symbol.replace]: (s, r) => s + r }, 'x')",
        "abcx",
    );
    assert_string(
        "'abc'.replaceAll({ [Symbol.replace]: (s, r) => r + s }, 'x')",
        "xabc",
    );
    assert_string(
        "'abc'.split({ [Symbol.split]: (s, n) => s + n }, 2)",
        "abc2",
    );
    assert_string("'abc'.search({ [Symbol.search]: s => s + '!' })", "abc!");
    assert_string("'abc'.match({ [Symbol.match]: s => s + '?' })", "abc?");
    assert_string(
        "'abc'.matchAll({ [Symbol.matchAll]: s => s + '*' })",
        "abc*",
    );
    // `this` is the argument, the receiver is passed before `ToString`.
    assert_bool(
        "let o = { [Symbol.search]() { return this === o } }; 'a'.search(o)",
        true,
    );
    assert_bool(
        "let r = new String('a'); let o = { [Symbol.match](s) { return s === r } }; \
         String.prototype.match.call(r, o)",
        true,
    );
    // An inherited hook is found through the prototype chain.
    assert_string(
        "class S { [Symbol.split](s) { return s.length } } String('abcd'.split(new S()))",
        "4",
    );
}

#[test]
fn string_method_hook_runs_before_to_string_of_this() {
    assert_string(
        "let log = []; let t = { toString() { log.push('this'); return 'abc' } }; \
         String.prototype.replace.call(t, { [Symbol.replace]() { log.push('hook'); return 1 } }, ''); \
         log.join()",
        "hook",
    );
}

#[test]
fn nullish_hook_falls_back_to_the_string_path() {
    assert_string(
        "'a[object Object]b'.replace({ [Symbol.replace]: undefined }, '-')",
        "a-b",
    );
    assert_number(
        "'xnully'.search({ [Symbol.search]: null, toString() { return 'null' } })",
        1.0,
    );
    assert_string(
        "'a,b'.split({ toString() { return ',' } }).join('|')",
        "a|b",
    );
}

#[test]
fn non_callable_hook_throws() {
    for (src, name) in [
        ("'abc'.match({ [Symbol.match]: 1 })", "match"),
        ("'abc'.matchAll({ [Symbol.matchAll]: 1 })", "matchAll"),
        ("'abc'.replace({ [Symbol.replace]: {} }, '')", "replace"),
        ("'abc'.search({ [Symbol.search]: true })", "search"),
        ("'abc'.split({ [Symbol.split]: 'x' })", "split"),
    ] {
        assert_uncaught(src, &format!("TypeError: Symbol.{name} is not a function"));
    }
}

#[test]
fn is_regexp_argument_throws_in_starts_with_ends_with_includes() {
    for name in ["startsWith", "endsWith", "includes"] {
        assert_uncaught(
            &format!("'a'.{name}({{ [Symbol.match]: true }})"),
            &format!(
                "TypeError: First argument to String.prototype.{name} must not be a regular expression"
            ),
        );
    }
    assert_uncaught(
        "'a'.startsWith({ [Symbol.match]: 1 })",
        "TypeError: First argument to String.prototype.startsWith must not be a regular expression",
    );
}

#[test]
fn falsy_match_hook_is_coerced_to_a_string() {
    assert_bool(
        "'[object Object]'.startsWith({ [Symbol.match]: false })",
        true,
    );
    assert_bool("'x[object Object]'.includes({ [Symbol.match]: 0 })", true);
    assert_bool(
        "'ab'.endsWith({ [Symbol.match]: '', toString() { return 'b' } })",
        true,
    );
}

#[test]
fn match_all_and_replace_all_require_a_global_regexp_argument() {
    let hooks = "[Symbol.matchAll]: () => 'm', [Symbol.replace]: () => 'r'";
    assert_uncaught(
        &format!("'a'.matchAll({{ [Symbol.match]: true, flags: 'i', {hooks} }})"),
        "TypeError: String.prototype.matchAll called with a non-global RegExp argument",
    );
    assert_uncaught(
        &format!("'a'.replaceAll({{ [Symbol.match]: true, flags: undefined, {hooks} }}, '')"),
        "TypeError: replaceAll must be called with a global RegExp",
    );
    assert_string(
        &format!("'a'.matchAll({{ [Symbol.match]: true, flags: 'gi', {hooks} }})"),
        "m",
    );
    assert_string(
        &format!(
            "'a'.replaceAll({{ [Symbol.match]: true, flags: {{ toString() {{ return 'g' }} }}, {hooks} }}, '')"
        ),
        "r",
    );
    // Not `IsRegExp`: `flags` is not read.
    assert_string(
        &format!("'a'.replaceAll({{ get flags() {{ throw 1 }}, {hooks} }}, '')"),
        "r",
    );
}
