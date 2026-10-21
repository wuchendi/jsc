//! Integration tests: `Map` and `Set` collection utilities via the CLI.
#![allow(clippy::expect_used)]

mod common;
use common::jsc;

/// Map/Set basics run inline with `-p` through the built binary.
///
/// Covers collections end to end: `Map` set/get sum, `has`, `size`, `Set`
/// add dedup + `size`, `Set` `has`, and `typeof (new Map())`.
#[test]
fn map_set_basics_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",40); m.set(\"b\",2); m.get(\"a\")+m.get(\"b\")"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",40); m.has(\"a\")"]),
        "true"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",40); m.set(\"b\",2); m.size"
        ]),
        "2"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let s=new Set(); s.add(1); s.add(1); s.add(2); s.size"
        ]),
        "2"
    );
    assert_eq!(
        jsc(&["-p", "let s=new Set(); s.add(1); s.add(2); s.has(2)"]),
        "true"
    );
    assert_eq!(jsc(&["-p", "typeof (new Map())"]), "object");
}

/// A `Map`'s `set` updates an existing key in place (no duplicate, `size`
/// unchanged) and a missing `get`/`has` yields `undefined`/`false`. Number and
/// string keys both compare by strict `===`.
#[test]
fn map_update_and_miss_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",1); m.set(\"a\",9); m.get(\"a\")"
        ]),
        "9"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",1); m.set(\"a\",9); m.size"
        ]),
        "1"
    );
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",1); m.get(\"x\")"]),
        "undefined"
    );
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",1); m.has(\"x\")"]),
        "false"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(1,\"x\"); m.set(2,\"y\"); m.get(1)+m.get(2)"
        ]),
        "xy"
    );
}

/// `delete` returns a boolean and compacts the entry out so a later `get`/`has`
/// misses and `size` shrinks; the surviving entries stay intact and a key can be
/// re-inserted afterward.
#[test]
fn map_delete_via_eval() {
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",1); m.delete(\"a\")"]),
        "true"
    );
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",1); m.delete(\"x\")"]),
        "false"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",1); m.delete(\"a\"); m.size"
        ]),
        "0"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",1); m.set(\"b\",2); m.set(\"c\",3); m.delete(\"b\"); \
             m.get(\"a\")+m.get(\"c\")+\"/\"+m.size"
        ]),
        "4/2"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let m=new Map(); m.set(\"a\",1); m.delete(\"a\"); m.set(\"a\",5); m.get(\"a\")"
        ]),
        "5"
    );
}

/// A `Set` dedups by strict `===` (adding an equal value is a no-op) and supports
/// `has`/`delete`/`size` over the shared backing.
#[test]
fn set_ops_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "let s=new Set(); s.add(1); s.add(2); s.delete(1); s.has(1)"
        ]),
        "false"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let s=new Set(); s.add(1); s.add(2); s.delete(1); s.size"
        ]),
        "1"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let s=new Set(); s.add(\"x\"); s.add(\"x\"); s.add(\"y\"); s.size"
        ]),
        "2"
    );
}

/// `set`/`add` return the collection (chainable) and empty/populated collections
/// render with their entry count.
#[test]
fn map_set_render_and_chain_via_eval() {
    assert_eq!(jsc(&["-p", "new Map()"]), "Map(0)");
    assert_eq!(jsc(&["-p", "new Set()"]), "Set(0)");
    assert_eq!(
        jsc(&["-p", "let s=new Set(); s.add(1); s.add(2); s"]),
        "Set(2)"
    );
    assert_eq!(
        jsc(&["-p", "let m=new Map(); m.set(\"a\",1).set(\"b\",2); m.size"]),
        "2"
    );
}

/// The Map/Set method names are shared with ordinary object/class methods, so a
/// non-collection receiver falls back to a plain user method call (`this`-bound).
#[test]
fn method_name_fallback_to_user_method() {
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.base=10} add(x){return this.base+x}} new C().add(5)"
        ]),
        "15"
    );
    assert_eq!(jsc(&["-p", "let o={get(){return 7}}; o.get()"]), "7");
}

/// The constructors take an iterable of entries / values.
#[test]
fn iterable_constructor_arguments() {
    assert_eq!(jsc(&["-p", "new Map([[1,2]]).get(1)"]), "2");
    assert_eq!(jsc(&["-p", "new Set([1,2,2]).size"]), "2");
}
