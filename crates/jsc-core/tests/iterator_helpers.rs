//! Integration tests: the `Iterator` global (DESIGN.md
//! §7.9): the abstract constructor, `Iterator.prototype` (=
//! `%IteratorPrototype%`) with its `constructor` / `@@toStringTag`
//! accessors, `Iterator.from` with its `%WrapForValidIteratorPrototype%`
//! wrappers, the lazy helpers over `%IteratorHelperPrototype%`, and the
//! eager helpers.

mod common;
use common::{assert_bool, assert_number, assert_string, assert_uncaught};

// --- the abstract constructor ----------------------------------------------------

#[test]
fn iterator_is_abstract() {
    assert_uncaught(
        "new Iterator()",
        "TypeError: Abstract class Iterator not directly constructable",
    );
    assert_bool(
        "try { Iterator(); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_string("typeof Iterator", "function");
    assert_number("Iterator.length", 0.0);
    assert_string("Iterator.name", "Iterator");
}

#[test]
fn a_subclass_constructs() {
    assert_bool(
        "class I extends Iterator {} const i = new I();
         i instanceof I && i instanceof Iterator && Object.getPrototypeOf(I.prototype) === Iterator.prototype",
        true,
    );
    assert_bool(
        "class I extends Iterator { constructor() { super(); this.x = 1; } } new I().x === 1",
        true,
    );
    // Instances inherit `[Symbol.iterator]() { return this }`.
    assert_bool(
        "class I extends Iterator {} new I()[Symbol.iterator]() instanceof I",
        true,
    );
}

// --- Iterator.prototype -------------------------------------------------------------

#[test]
fn iterator_prototype_is_the_shared_iterator_prototype() {
    assert_bool(
        "Iterator.prototype === Object.getPrototypeOf(Object.getPrototypeOf([].values()))",
        true,
    );
    assert_bool(
        "function* g() {}
         Object.getPrototypeOf(Object.getPrototypeOf(Object.getPrototypeOf(g()))) === Iterator.prototype",
        true,
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Iterator, 'prototype');
         !d.writable && !d.enumerable && !d.configurable",
        true,
    );
    assert_bool("[].values() instanceof Iterator", true);
}

#[test]
fn constructor_and_to_string_tag_are_accessors() {
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Iterator.prototype, 'constructor');
         typeof d.get === 'function' && typeof d.set === 'function' && !d.enumerable && d.configurable",
        true,
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Iterator.prototype, Symbol.toStringTag);
         typeof d.get === 'function' && typeof d.set === 'function' && !d.enumerable && d.configurable",
        true,
    );
    assert_bool("Iterator.prototype.constructor === Iterator", true);
    assert_bool("[].values().constructor === Iterator", true);
    assert_string("Iterator.prototype[Symbol.toStringTag]", "Iterator");
    assert_string(
        "Object.prototype.toString.call(Iterator.prototype)",
        "[object Iterator]",
    );
    assert_string(
        "class I extends Iterator {} Object.prototype.toString.call(new I())",
        "[object Iterator]",
    );
}

#[test]
fn the_accessor_setters_ignore_the_prototype() {
    // On %Iterator.prototype% itself: TypeError.
    assert_bool(
        "try { Iterator.prototype.constructor = 1; false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Iterator.prototype[Symbol.toStringTag] = 'x'; false }
         catch (e) { e instanceof TypeError }",
        true,
    );
    // On an inheriting object: an own data property, the prototype untouched.
    assert_bool(
        "class I extends Iterator {} const i = new I(); i.constructor = 1;
         const d = Object.getOwnPropertyDescriptor(i, 'constructor');
         d.value === 1 && d.writable && d.enumerable && d.configurable
           && Iterator.prototype.constructor === Iterator",
        true,
    );
    assert_string(
        "class I extends Iterator {} const i = new I(); i[Symbol.toStringTag] = 'Mine';
         Object.prototype.toString.call(i) + Iterator.prototype[Symbol.toStringTag]",
        "[object Mine]Iterator",
    );
    // A non-object receiver: TypeError.
    assert_bool(
        "const set = Object.getOwnPropertyDescriptor(Iterator.prototype, 'constructor').set;
         try { set.call(1, 2); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

// --- Iterator.from ----------------------------------------------------------------

#[test]
fn from_an_iterable_returns_its_iterator() {
    assert_string(
        "const it = Iterator.from([1]); JSON.stringify([...it])",
        "[1]",
    );
    assert_bool("const it = [1].values(); Iterator.from(it) === it", true);
    assert_bool(
        "const it = Iterator.from([1]); it[Symbol.iterator]() === it && it instanceof Iterator",
        true,
    );
    assert_number("Iterator.from.length", 1.0);
}

#[test]
fn from_a_plain_iterator_wraps_it() {
    assert_string(
        "let n = 0;
         const src = { next() { n += 1; return { value: n, done: n > 2 }; } };
         const w = Iterator.from(src);
         JSON.stringify([w !== src, w instanceof Iterator, [...w]])",
        "[true,true,[1,2]]",
    );
    // `next` is read once, when wrapping.
    assert_number(
        "let reads = 0;
         const src = { get next() { reads += 1; return () => ({ done: true }); } };
         const w = Iterator.from(src); w.next(); w.next(); reads",
        1.0,
    );
    // The wrapper's prototype holds `next` / `return` and inherits
    // %Iterator.prototype%.
    assert_bool(
        "const w = Iterator.from({ next() { return { done: true }; } });
         const p = Object.getPrototypeOf(w);
         p !== Iterator.prototype && Object.getPrototypeOf(p) === Iterator.prototype
           && p.hasOwnProperty('next') && p.hasOwnProperty('return')",
        true,
    );
}

#[test]
fn the_wrapper_forwards_return() {
    assert_string(
        "const calls = [];
         const src = { next() { return { value: 1, done: false }; },
                       return(...a) { calls.push(a.length); return { value: 'r', done: true }; } };
         const w = Iterator.from(src);
         const r = w.return();
         JSON.stringify([r, calls])",
        "[{\"value\":\"r\",\"done\":true},[0]]",
    );
    // Without a `return`: `{ value: undefined, done: true }`.
    assert_string(
        "const w = Iterator.from({ next() { return { done: false }; } });
         JSON.stringify(w.return())",
        "{\"done\":true}",
    );
    // `break` out of a for-of over the wrapper closes the source.
    assert_number(
        "let closed = 0;
         const w = Iterator.from({ next() { return { value: 1, done: false }; },
                                   return() { closed += 1; return {}; } });
         for (const x of w) { break; }
         closed",
        1.0,
    );
    assert_bool(
        "const w = Iterator.from({ next() { return { done: true }; } });
         const next = Object.getPrototypeOf(w).next;
         try { next.call({}); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

#[test]
fn from_iterates_strings_and_rejects_other_primitives() {
    assert_string("JSON.stringify([...Iterator.from('ab')])", "[\"a\",\"b\"]");
    assert_bool(
        "Object.prototype.toString.call(Iterator.from('a')) === '[object String Iterator]'",
        true,
    );
    for primitive in ["1", "true", "Symbol()", "undefined", "null"] {
        assert_bool(
            &format!(
                "try {{ Iterator.from({primitive}); false }} catch (e) {{ e instanceof TypeError }}"
            ),
            true,
        );
    }
    // A non-callable `@@iterator`, or one returning a primitive: TypeError.
    assert_bool(
        "try { Iterator.from({ [Symbol.iterator]: 1 }); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Iterator.from({ [Symbol.iterator]() { return 1; } }); false }
         catch (e) { e instanceof TypeError }",
        true,
    );
}

// --- lazy helpers ---------------------------------------------------------------------

/// A source iterator over `0, 1, 2, ...` (up to `n`) logging its `next` and
/// `return` calls into `log`.
const LOGGED: &str = "const log = [];
     function src(n) {
       let i = 0;
       return Object.setPrototypeOf({
         next() { log.push('next'); return i < n ? { value: i++, done: false } : { value: undefined, done: true }; },
         return() { log.push('return'); return {}; },
       }, Iterator.prototype);
     }";

#[test]
fn helpers_produce_their_values() {
    assert_string(
        "JSON.stringify([...[1, 2, 3].values().map((x, i) => x * 10 + i)])",
        "[10,21,32]",
    );
    assert_string(
        "JSON.stringify([...[1, 2, 3, 4].values().filter((x, i) => x % 2 === 0 && i > 0)])",
        "[2,4]",
    );
    assert_string("JSON.stringify([...[1, 2, 3].values().take(2)])", "[1,2]");
    assert_string("JSON.stringify([...[1, 2, 3].values().drop(1)])", "[2,3]");
    assert_string(
        "JSON.stringify([...[1, 2].values().flatMap(x => [x, x * 10])])",
        "[1,10,2,20]",
    );
    assert_string(
        "JSON.stringify([...[1, 2, 3].values().drop(1).map(x => x * 2).take(1)])",
        "[4]",
    );
    assert_string(
        "JSON.stringify([...[1].values().take(Infinity)].concat([...[1, 2].values().drop(Infinity)]))",
        "[1]",
    );
}

#[test]
fn helper_prototype_shape() {
    assert_bool(
        "const p = Object.getPrototypeOf([].values().map(x => x));
         Object.getPrototypeOf(p) === Iterator.prototype
           && typeof p.next === 'function' && typeof p.return === 'function'
           && p[Symbol.toStringTag] === 'Iterator Helper'",
        true,
    );
    assert_string(
        "Object.prototype.toString.call([].values().filter(x => x))",
        "[object Iterator Helper]",
    );
    assert_bool(
        "Iterator.prototype.map.length === 1 && Iterator.prototype.flatMap.name === 'flatMap'",
        true,
    );
    assert_bool(
        "const r = [1].values().map(x => x).next(); r.value === 1 && r.done === false",
        true,
    );
}

#[test]
fn helpers_are_lazy() {
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(3).map(x => x);
             const before = log.length;
             h.next();
             before + ':' + log.join(',')"
        ),
        "0:next",
    );
    for helper in [
        "filter(x => true)",
        "take(1)",
        "drop(1)",
        "flatMap(x => [x])",
    ] {
        assert_number(&format!("{LOGGED} src(3).{helper}; log.length"), 0.0);
    }
}

#[test]
fn take_closes_the_source_at_the_limit() {
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(5).take(2);
             h.next(); h.next();
             const r = h.next();
             log.join(',') + ':' + r.done + ':' + h.next().done"
        ),
        "next,next,return:true:true",
    );
}

#[test]
fn limits_are_range_checked_after_to_number() {
    assert_bool(
        "try { [].values().drop(-1); false } catch (e) { e instanceof RangeError }",
        true,
    );
    assert_bool(
        "try { [].values().take(NaN); false } catch (e) { e instanceof RangeError }",
        true,
    );
    // `ToNumber` runs first (its `valueOf` is observed), then the source is
    // closed under the RangeError.
    assert_string(
        &format!(
            "{LOGGED}
             let e;
             try {{ src(1).take({{ valueOf() {{ log.push('valueOf'); return NaN; }} }}); }}
             catch (x) {{ e = x; }}
             (e instanceof RangeError) + ':' + log.join(',')"
        ),
        "true:valueOf,return",
    );
    assert_string(
        &format!(
            "{LOGGED}
             let e;
             try {{ src(1).drop(-1); }} catch (x) {{ e = x; }}
             (e instanceof RangeError) + ':' + log.join(',')"
        ),
        "true:return",
    );
    // A fractional limit truncates; `-0.5` is `0`.
    assert_string(
        "JSON.stringify([...[1, 2, 3].values().take(1.9)].concat([...[1].values().take(-0.5)]))",
        "[1]",
    );
}

#[test]
fn a_non_callable_callback_throws_before_any_step() {
    for helper in ["map", "filter", "flatMap"] {
        assert_string(
            &format!(
                "{LOGGED}
                 let e;
                 try {{ src(2).{helper}(1); }} catch (x) {{ e = x; }}
                 (e instanceof TypeError) + ':' + log.join(',')"
            ),
            "true:return",
        );
    }
    // `this` must be an object; nothing else is read.
    assert_bool(
        "try { Iterator.prototype.map.call(1, x => x); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

#[test]
fn a_throwing_callback_closes_the_source() {
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(3).map(() => {{ throw 7; }});
             let e;
             try {{ h.next(); }} catch (x) {{ e = x; }}
             e + ':' + log.join(',') + ':' + h.next().done"
        ),
        "7:next,return:true",
    );
}

#[test]
fn flat_map_rejects_a_string_result() {
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(2).flatMap(x => 'ab');
             let e;
             try {{ h.next(); }} catch (x) {{ e = x; }}
             (e instanceof TypeError) + ':' + log.join(',')"
        ),
        "true:next,return",
    );
    // An iterable object (a String wrapper included) is flattened.
    assert_string(
        "JSON.stringify([...[1].values().flatMap(() => new String('ab'))])",
        "[\"a\",\"b\"]",
    );
}

#[test]
fn next_while_running_throws() {
    assert_bool(
        "let h; h = [1].values().map(() => h.next());
         try { h.next(); false } catch (e) { e instanceof TypeError }",
        true,
    );
    // The helper completed under that throw.
    assert_bool(
        "let h; h = [1, 2].values().map(() => h.next());
         try { h.next(); } catch (e) {}
         h.next().done",
        true,
    );
}

#[test]
fn return_before_start_closes_the_source() {
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(3).map(x => x);
             const r = h.return();
             log.join(',') + ':' + r.done + ':' + (r.value === undefined) + ':' + h.next().done + ':' + log.length"
        ),
        "return:true:true:true:1",
    );
    // After a yield `return()` closes the source too; a completed helper does
    // not close again.
    assert_string(
        &format!(
            "{LOGGED}
             const h = src(3).filter(x => true);
             h.next();
             h.return();
             h.return();
             log.join(',')"
        ),
        "next,return",
    );
    // `flatMap` closes the open inner iterator, then the source.
    assert_string(
        &format!(
            "{LOGGED}
             const inner = Object.setPrototypeOf({{
               next() {{ return {{ value: 'i', done: false }}; }},
               return() {{ log.push('inner'); return {{}}; }},
             }}, Iterator.prototype);
             const h = src(3).flatMap(() => inner);
             h.next().value + ':' + (h.return(), log.join(','))"
        ),
        "i:next,inner,return",
    );
}

#[test]
fn helper_methods_reject_other_receivers() {
    assert_bool(
        "const { next } = Object.getPrototypeOf([].values().map(x => x));
         try { next.call([].values()); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "const p = Object.getPrototypeOf([].values().map(x => x));
         try { p.return.call(Iterator.from({ next() {} })); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

// --- eager helpers ------------------------------------------------------------------

#[test]
fn eager_helpers_produce_their_results() {
    assert_number("[1, 2, 3].values().reduce((a, x) => a + x)", 6.0);
    assert_number("[1, 2, 3].values().reduce((a, x) => a + x, 10)", 16.0);
    assert_string(
        "JSON.stringify([5, 6].values().reduce((a, x, i) => a.concat([[x, i]]), []))",
        "[[5,0],[6,1]]",
    );
    // Without an initial value the counter starts at 1.
    assert_string(
        "JSON.stringify([5, 6, 7].values().reduce((a, x, i) => a + ',' + x + ':' + i))",
        "\"5,6:1,7:2\"",
    );
    assert_string(
        "JSON.stringify([1, 2, 3].values().map(x => x * 2).toArray())",
        "[2,4,6]",
    );
    assert_bool("Array.isArray([].values().toArray())", true);
    assert_string(
        "const out = []; const r = [1, 2].values().forEach((x, i) => out.push(x + ':' + i));
         out.join(',') + ':' + r",
        "1:0,2:1:undefined",
    );
    assert_bool("[1, 2, 3].values().some(x => x === 2)", true);
    assert_bool("[1, 2, 3].values().some(x => x === 4)", false);
    assert_bool("[1, 2, 3].values().every(x => x > 0)", true);
    assert_bool("[1, 2, 3].values().every(x => x < 2)", false);
    assert_number("[1, 2, 3].values().find((x, i) => i === 1)", 2.0);
    assert_bool("[1, 2, 3].values().find(x => x > 3) === undefined", true);
    assert_bool(
        "const p = Iterator.prototype;
         p.reduce.length === 1 && p.toArray.length === 0 && p.forEach.length === 1
           && p.some.length === 1 && p.every.length === 1 && p.find.length === 1
           && p.toArray.name === 'toArray' && !Object.getOwnPropertyDescriptor(p, 'find').enumerable",
        true,
    );
}

#[test]
fn some_every_find_close_on_an_early_result() {
    for (call, result) in [
        ("some(x => x === 1)", "true"),
        ("every(x => x === 0)", "false"),
        ("find(x => x === 1)", "1"),
    ] {
        assert_string(
            &format!(
                "{LOGGED}
                 const r = src(5).{call};
                 r + ':' + log.join(',')"
            ),
            &format!("{result}:next,next,return"),
        );
    }
    // Running to the end does not close.
    assert_string(
        &format!(
            "{LOGGED}
             const r = src(2).some(x => false);
             r + ':' + log.join(',')"
        ),
        "false:next,next,next",
    );
}

#[test]
fn a_throwing_eager_callback_closes_the_iterator() {
    for helper in ["reduce", "forEach", "some", "every", "find"] {
        assert_string(
            &format!(
                "{LOGGED}
                 let e;
                 try {{ src(3).{helper}(() => {{ throw 7; }}, 0); }} catch (x) {{ e = x; }}
                 e + ':' + log.join(',')"
            ),
            "7:next,return",
        );
    }
    // The callback's throw wins over one from `return`.
    assert_number(
        "const it = Object.setPrototypeOf({
           next() { return { value: 1, done: false }; },
           return() { throw 2; },
         }, Iterator.prototype);
         let e;
         try { it.forEach(() => { throw 1; }); } catch (x) { e = x; }
         e",
        1.0,
    );
    // A throwing `next` propagates without closing.
    assert_string(
        "const log = [];
         const it = Object.setPrototypeOf({
           next() { throw 3; },
           return() { log.push('return'); return {}; },
         }, Iterator.prototype);
         let e;
         try { it.toArray(); } catch (x) { e = x; }
         e + ':' + log.length",
        "3:0",
    );
}

#[test]
fn eager_helpers_validate_before_stepping() {
    for helper in ["reduce", "forEach", "some", "every", "find"] {
        assert_string(
            &format!(
                "{LOGGED}
                 let e;
                 try {{ src(2).{helper}(1); }} catch (x) {{ e = x; }}
                 (e instanceof TypeError) + ':' + log.join(',')"
            ),
            "true:return",
        );
    }
    assert_bool(
        "try { Iterator.prototype.toArray.call(1); false } catch (e) { e instanceof TypeError }",
        true,
    );
    // `next` is read once.
    assert_number(
        "let reads = 0, i = 0;
         const it = Object.setPrototypeOf({
           get next() { reads++; return () => ({ value: i, done: i++ > 2 }); },
         }, Iterator.prototype);
         it.toArray();
         reads",
        1.0,
    );
}

#[test]
fn reduce_of_an_empty_iterator_without_initial_value_throws() {
    assert_uncaught(
        "[].values().reduce((a, x) => a + x)",
        "TypeError: Reduce of empty iterator with no initial value",
    );
    assert_bool(
        "[].values().reduce((a, x) => a + x, undefined) === undefined",
        true,
    );
}
