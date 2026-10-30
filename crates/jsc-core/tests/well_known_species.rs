//! Integration tests: the honoured `@@species`, `@@isConcatSpreadable` and
//! `@@unscopables` (`design/symbol-strings.md` §4): `ArraySpeciesCreate` and
//! `SpeciesConstructor` read `C[@@species]`, the builtin
//! `get [Symbol.species]` accessors, `concat`'s `IsConcatSpreadable`, and the
//! `Array.prototype[@@unscopables]` list object.

mod common;
use common::{assert_bool, assert_output, assert_string, assert_uncaught};

/// The methods that create their result by `ArraySpeciesCreate`.
const SPECIES_CALLS: [&str; 7] = [
    "map((x) => x)",
    "filter(() => true)",
    "slice(0)",
    "splice(0, 1)",
    "concat([3])",
    "flat()",
    "flatMap((x) => [x])",
];

// --- @@species ------------------------------------------------------------------

#[test]
fn species_getter_returns_plain_arrays_from_a_subclass() {
    let sub = "class A extends Array { static get [Symbol.species]() { return Array } }";
    for call in SPECIES_CALLS {
        assert_bool(
            &format!(
                "{sub} const a = A.from([1, 2]); const r = a.{call}; \
                 Array.isArray(r) && !(r instanceof A) && r.constructor === Array"
            ),
            true,
        );
    }
}

#[test]
fn subclass_without_override_inherits_the_builtin_species() {
    for call in SPECIES_CALLS {
        assert_bool(
            &format!("class A extends Array {{}} A.from([1, 2]).{call} instanceof A"),
            true,
        );
    }
}

#[test]
fn null_or_undefined_species_creates_a_plain_array() {
    for value in ["null", "undefined"] {
        for call in SPECIES_CALLS {
            assert_bool(
                &format!(
                    "class A extends Array {{ static get [Symbol.species]() {{ return {value} }} }} \
                     const r = A.from([1, 2]).{call}; Array.isArray(r) && !(r instanceof A)"
                ),
                true,
            );
        }
    }
    // A `constructor` whose `@@species` is absent also creates a plain array.
    assert_bool(
        "const a = [1]; a.constructor = { [Symbol.species]: undefined }; \
         const r = a.map((x) => x); Array.isArray(r) && r.constructor === Array",
        true,
    );
}

#[test]
fn non_constructor_species_throws() {
    for species in ["1", "{}", "() => {}"] {
        for call in SPECIES_CALLS {
            assert_uncaught(
                &format!(
                    "const a = [1, 2]; a.constructor = {{ [Symbol.species]: {species} }}; a.{call}"
                ),
                "TypeError: object.constructor[Symbol.species] is not a constructor",
            );
        }
    }
}

#[test]
fn user_species_constructs_with_the_length() {
    assert_string(
        "let seen = []; function S(n) { seen.push(n); } \
         const a = [1, 2, 3]; a.constructor = { [Symbol.species]: S }; \
         a.map((x) => x); a.filter(() => true); a.slice(1); a.concat(); \
         seen.join()",
        "3,0,2,0",
    );
    assert_bool(
        "function S() {} const a = [1, 2]; a.constructor = { [Symbol.species]: S }; \
         a.map((x) => x) instanceof S",
        true,
    );
}

#[test]
fn modified_array_species_is_honoured_by_plain_arrays() {
    // `%Array%[@@species]` replaced: every plain array's `ArraySpeciesCreate`
    // (including the inline `map` / `filter` call sites) sees it.
    let setup = "function S() {} \
        Object.defineProperty(Array, Symbol.species, { get() { return S } });";
    for call in SPECIES_CALLS {
        assert_bool(&format!("{setup} [1, 2].{call} instanceof S"), true);
    }
    // Deleted, then redefined as a data property, without any
    // prototype-reaching name in the program.
    assert_bool(
        "function S() {} delete Array[Symbol.species]; Array[Symbol.species] = S; \
         [1, 2].map((x) => x) instanceof S && [1].filter((x) => x) instanceof S",
        true,
    );
    // Deleted only: `Get(%Array%, @@species)` is `undefined`, a plain array.
    assert_bool(
        "delete Array[Symbol.species]; Array.isArray([1].map((x) => x))",
        true,
    );
}

#[test]
fn builtin_species_getters() {
    for ctor in ["Array", "Map", "Set", "Promise"] {
        let desc = format!("Object.getOwnPropertyDescriptor({ctor}, Symbol.species)");
        assert_bool(&format!("{ctor}[Symbol.species] === {ctor}"), true);
        assert_bool(
            &format!(
                "const d = {desc}; d.set === undefined && !d.enumerable && d.configurable \
                 && typeof d.get === 'function'"
            ),
            true,
        );
        assert_string(&format!("{desc}.get.name"), "get [Symbol.species]");
        assert_bool(&format!("{desc}.get.length === 0"), true);
    }
    // The getter returns its receiver.
    assert_bool(
        "const get = Object.getOwnPropertyDescriptor(Array, Symbol.species).get; \
         const o = {}; get.call(o) === o",
        true,
    );
    assert_bool("class M extends Map {} M[Symbol.species] === M", true);
}

#[test]
fn promise_subclass_then_uses_species() {
    assert_output(
        "class P extends Promise {} \
         const p = new P((r) => r(1)).then((v) => v + 1); \
         console.log(p instanceof P); \
         p.then((v) => console.log(v));",
        "true\n2\n",
    );
    assert_output(
        "class P extends Promise { static get [Symbol.species]() { return Promise } } \
         const p = new P((r) => r(1)).then((v) => v); \
         console.log(p instanceof P, p instanceof Promise);",
        "false true\n",
    );
    // A user species constructor builds the capability.
    assert_output(
        "let made = 0; \
         class S extends Promise { constructor(e) { made++; super(e); } } \
         const p = Promise.resolve(1); p.constructor = { [Symbol.species]: S }; \
         const q = p.then((v) => v * 3); \
         console.log(q instanceof S, made); \
         q.then((v) => console.log(v));",
        "true 1\n3\n",
    );
    // A throwing handler rejects the species promise; `catch` goes through `then`.
    assert_output(
        "class P extends Promise {} \
         const q = new P((r) => r(1)).then(() => { throw 'boom' }); \
         console.log(q instanceof P); \
         q.catch((e) => console.log('caught', e));",
        "true\ncaught boom\n",
    );
}

#[test]
fn promise_species_errors() {
    assert_uncaught(
        "const p = Promise.resolve(1); p.constructor = 1; p.then()",
        "TypeError: object.constructor is not an object",
    );
    assert_uncaught(
        "const p = Promise.resolve(1); p.constructor = { [Symbol.species]: 1 }; p.then()",
        "TypeError: object.constructor[Symbol.species] is not a constructor",
    );
    assert_uncaught(
        "function C(e) {} const p = Promise.resolve(1); \
         p.constructor = { [Symbol.species]: C }; p.then()",
        "TypeError: Promise resolve or reject function is not callable",
    );
}

#[test]
fn promise_subclass_finally_uses_species() {
    assert_output(
        "class P extends Promise {} \
         const p = new P((r) => r(1)).finally(() => console.log('fin')); \
         console.log(p instanceof P); \
         p.then((v) => console.log(v));",
        "true\nfin\n1\n",
    );
    assert_output(
        "class P extends Promise {} \
         new P((_, r) => r('no')).finally(() => console.log('fin')).catch((e) => console.log('caught', e));",
        "fin\ncaught no\n",
    );
    // `finally` on a thenable calls its `then` with the two closures.
    assert_output(
        "const t = { then(f, r) { console.log(typeof f, typeof r, f.length); return 7 } }; \
         console.log(Promise.prototype.finally.call(t, () => {}));",
        "function function 1\n7\n",
    );
}

// --- @@isConcatSpreadable --------------------------------------------------------

#[test]
fn concat_spreads_by_is_concat_spreadable() {
    assert_string(
        "const o = { length: 2, 0: 1, 1: 2, [Symbol.isConcatSpreadable]: true }; \
         JSON.stringify([0].concat(o))",
        "[0,1,2]",
    );
    assert_string(
        "const a = [1, 2]; a[Symbol.isConcatSpreadable] = false; \
         const r = [0].concat(a); r.length + ':' + (r[1] === a)",
        "2:true",
    );
    // The receiver is tested too, and the value goes through `ToBoolean`.
    assert_string(
        "const a = [1, 2]; a[Symbol.isConcatSpreadable] = 0; \
         const r = a.concat([3]); r.length + ':' + (r[0] === a)",
        "2:true",
    );
    assert_string(
        "const o = { length: 1, 0: 'x', [Symbol.isConcatSpreadable]: 'yes' }; \
         JSON.stringify(Array.prototype.concat.call(o, o))",
        "[\"x\",\"x\"]",
    );
    // An inherited hook counts.
    assert_string(
        "class L { constructor() { this.length = 1; this[0] = 'y' } \
         get [Symbol.isConcatSpreadable]() { return true } } \
         JSON.stringify([].concat(new L()))",
        "[\"y\"]",
    );
}

// --- @@unscopables ---------------------------------------------------------------

#[test]
fn array_prototype_unscopables() {
    let u = "Array.prototype[Symbol.unscopables]";
    assert_bool(&format!("Object.getPrototypeOf({u}) === null"), true);
    assert_bool(&format!("{u}.flat === true"), true);
    assert_string(
        &format!("Object.keys({u}).join()"),
        "at,copyWithin,entries,fill,find,findIndex,findLast,findLastIndex,flat,flatMap,\
         includes,keys,toReversed,toSorted,toSpliced,values",
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.unscopables); \
         !d.writable && !d.enumerable && d.configurable",
        true,
    );
    assert_bool(
        &format!(
            "const d = Object.getOwnPropertyDescriptor({u}, 'flat'); \
             d.value === true && d.writable && d.enumerable && d.configurable"
        ),
        true,
    );
}

// --- Builtin accessors in accessor-free programs ---------------------------------
//
// None of these sources builds or names an accessor, so `__get` is compiled
// without getter dispatch; the builtin `get [@@species]` /
// `[@@toStringTag]` / `description` getters must still be called.

#[test]
fn plain_array_species_methods_without_accessors() {
    assert_string("[1, 2, 3, 4].slice(1, 3).join()", "2,3");
    assert_string("[1, 2, 3].map((x) => x * 2).join()", "2,4,6");
    assert_string("[1, 2, 3, 4].filter((x) => x % 2).join()", "1,3");
    assert_string("[1].concat([2, 3], 4).join()", "1,2,3,4");
    assert_string(
        "const a = [1, 2, 3, 4]; a.splice(1, 2).join() + '|' + a.join()",
        "2,3|1,4",
    );
    assert_string("[1, [2, [3]]].flat().length + ''", "3");
    assert_string("[1, 2].flatMap((x) => [x, x]).join()", "1,1,2,2");
    assert_bool("Array.isArray([1, 2, 3].slice(1))", true);
}

#[test]
fn promise_then_without_accessors() {
    assert_output("Promise.resolve(1).then((v) => console.log(v + 1));", "2\n");
    // Resolving with a promise runs its `then` (a `SpeciesConstructor` read)
    // without the program naming it.
    assert_output(
        "const f = async () => Promise.resolve(5); \
         (async () => console.log(await f()))();",
        "5\n",
    );
}

#[test]
fn builtin_tags_and_getters_without_accessors() {
    assert_string(
        "Object.prototype.toString.call([].values())",
        "[object Array Iterator]",
    );
    assert_string("Symbol(\"d\").description", "d");
    assert_bool("[].values().constructor === Iterator", true);
}
