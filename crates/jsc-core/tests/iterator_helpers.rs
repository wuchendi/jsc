//! Integration tests: the `Iterator` global (`design/symbol-strings.md`
//! §5.7): the abstract constructor, `Iterator.prototype` (=
//! `%IteratorPrototype%`) with its `constructor` / `@@toStringTag`
//! accessors, and `Iterator.from` with its `%WrapForValidIteratorPrototype%`
//! wrappers.

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
