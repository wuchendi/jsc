//! Tagged templates `` tag`a${x}b` `` (ECMA-262 13.3.11): the tag is called
//! with the site's cached, frozen template object (cooked strings plus a
//! non-writable `raw`) and then the substitution values.

mod common;

use common::{assert_bool, assert_output, assert_rejected, assert_str, assert_uncaught};

#[test]
fn passes_cooked_strings_and_substitutions() {
    assert_str(
        "function tag(s, ...v) { return s.join('|') + ':' + v.join(','); } tag`a${1}b${2}c`",
        "a|b|c:1,2",
    );
    assert_str(
        "function tag(s, ...v) { return s.length + ':' + v.length; } tag`${1}${2}`",
        "3:2",
    );
    assert_str("function tag(s) { return s[0]; } tag`a\\nb`", "a\nb");
}

#[test]
fn raw_strings_keep_source_text() {
    assert_str(
        r"function tag(s) { return s.raw.join('|'); } tag`a\nb${0}\x41`",
        r"a\nb|\x41",
    );
    assert_bool(
        "function tag(s) { return s.raw.length === s.length; } tag`a${1}b${2}`",
        true,
    );
}

#[test]
fn raw_normalizes_line_terminators() {
    assert_bool(
        "function tag(s) { return s.raw[0] === 'a\\nb\\nc' && s[0] === 'a\\nb\\nc'; } tag`a\r\nb\rc`",
        true,
    );
}

#[test]
fn template_object_is_frozen() {
    assert_bool(
        "function tag(s) { return s; } let s = tag`a${1}b`; \
         Array.isArray(s) && Array.isArray(s.raw) && Object.isFrozen(s) && Object.isFrozen(s.raw)",
        true,
    );
    assert_bool(
        "function tag(s) { return s; } let d = Object.getOwnPropertyDescriptor(tag`x`, 'raw'); \
         !d.writable && !d.enumerable && !d.configurable",
        true,
    );
    assert_str(
        "function tag(s) { return s; } Object.keys(tag`a${1}b`).join()",
        "0,1",
    );
}

#[test]
fn same_site_yields_same_object() {
    assert_bool(
        "function tag(s) { return s; } let a = []; \
         for (let i = 0; i < 3; i++) a.push(tag`x${i}`); a[0] === a[1] && a[1] === a[2]",
        true,
    );
    assert_bool(
        "function tag(s) { return s; } function f() { return tag`x`; } f() === f()",
        true,
    );
}

#[test]
fn distinct_sites_yield_distinct_objects() {
    assert_bool("function tag(s) { return s; } tag`x` !== tag`x`", true);
}

#[test]
fn invalid_escape_cooks_to_undefined() {
    assert_bool(
        r"function tag(s) { return s[0] === undefined && s.raw[0] === '\\unicode'; } tag`\unicode`",
        true,
    );
    assert_bool(
        r"function tag(s) { return s[0] === undefined && s.raw[0] === '\\01'; } tag`\01`",
        true,
    );
    assert_bool(
        r"function tag(s) { return s[0] === undefined && s.raw[0] === '\\xg' && s[1] === 'ok'; } tag`\xg${0}ok`",
        true,
    );
}

#[test]
fn invalid_escape_in_untagged_template_is_rejected() {
    assert_rejected(r"`\unicode`");
}

#[test]
fn member_tag_binds_this() {
    assert_str(
        "let o = { v: 7, f(s, x) { return this.v + x; } }; String(o.f`a${1}`)",
        "8",
    );
    assert_str(
        "let o = { v: 'k', f(s) { return this.v + s[0]; } }; o['f']`x` + (o.f)`y`",
        "kxky",
    );
}

#[test]
fn super_member_tag() {
    assert_str(
        "class A { m(s) { return s[0] + this.k; } } \
         class B extends A { constructor() { super(); this.k = '!'; } n() { return super.m`hi`; } } \
         new B().n()",
        "hi!",
    );
}

#[test]
fn tag_runs_before_substitutions() {
    assert_output(
        "let log = []; function t(x) { log.push(x); return x; } \
         function g() { log.push('tag'); return (s, ...v) => v.join(); } \
         let r = g()`${t(1)}-${t(2)}`; console.log(log.join(), r)",
        "tag,1,2 1,2\n",
    );
}

#[test]
fn non_callable_tag_throws() {
    assert_uncaught("let x = 1; x`a`", "TypeError: x is not a function");
    assert_uncaught("let o = {}; o.f`a`", "TypeError: o.f is not a function");
}

#[test]
fn chained_and_new_tags() {
    assert_str(
        "function t(s) { return (s2) => s[0] + s2[0]; } t`a``b`",
        "ab",
    );
    // `new tag`x`` is `new (tag`x`)`: the tag is called (strict, so `this` is
    // `undefined`) and its result is the constructor.
    assert_str(
        "let seen = []; function C(a) { this.a = a; } \
         function tag(s) { seen.push(s[0]); return C; } \
         let i = new tag`x`; let j = new tag`y`(1); \
         [i instanceof C, i.a, j instanceof C, j.a, seen.join('')].join()",
        "true,,true,1,xy",
    );
    assert_uncaught(
        "function C(s) { this.s = s[0]; } new C`x`",
        "TypeError: Cannot set properties of undefined (setting 's')",
    );
}

#[test]
fn optional_chain_tag_is_rejected() {
    assert_rejected("let a = { b(s) { return s; } }; a?.b`x`");
}

#[test]
fn substitution_may_yield() {
    assert_str(
        "function tag(s, ...v) { return s.join('|') + ':' + v.join(); } \
         function* g() { return tag`a${yield 1}b`; } \
         let it = g(); it.next(); it.next(5).value",
        "a|b:5",
    );
}

#[test]
fn substitution_may_await() {
    assert_output(
        "function tag(s, ...v) { return s.join('|') + ':' + v.join(); } \
         async function f() { return tag`a${await Promise.resolve(3)}b`; } \
         f().then((r) => console.log(r))",
        "a|b:3\n",
    );
}

#[test]
fn string_raw() {
    assert_str(r"String.raw`a\n${1}b`", r"a\n1b");
    assert_str("String.raw({ raw: ['x', 'y', 'z'] }, 1, 2, 3)", "x1y2z");
}
