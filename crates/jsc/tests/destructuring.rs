//! Integration tests: array and object destructuring in `let`/`const`/`var`
//! declarations, exercised end to end through the built `jsc` binary.

mod common;
use common::jsc;

/// Array destructuring in declarations steps the initializer's iterator —
/// plain elements, holes, defaults, rest, `const`, and nested patterns.
#[test]
fn array_destructuring_declarations_via_eval() {
    // Plain two-element bind.
    assert_eq!(jsc(&["-p", "let [a,b] = [1,2]; a+b"]), "3");
    // A hole skips its index.
    assert_eq!(jsc(&["-p", "let [,b] = [1,2]; b"]), "2");
    // A default fills a missing (undefined) slot.
    assert_eq!(jsc(&["-p", "let [a=9] = []; a"]), "9");
    // A rest collects the remaining elements into a new array.
    assert_eq!(jsc(&["-p", "let [x, ...r] = [1,2,3]; r.length"]), "2");
    // `const` bindings destructure the same way.
    assert_eq!(jsc(&["-p", "const [a,b]=[3,4]; a+b"]), "7");
    // A nested pattern recurses into a sub-array.
    assert_eq!(jsc(&["-p", "let [[a]] = [[5]]; a"]), "5");
}

/// Object destructuring in declarations desugars to keyed reads
/// off the initializer — shorthand binds, renamed binds, defaults, a rest that
/// collects the remaining own keys into a new object, and nested patterns.
#[test]
fn object_destructuring_declarations_via_eval() {
    // Shorthand two-key bind.
    assert_eq!(jsc(&["-p", "let {x,y} = {x:40,y:2}; x+y"]), "42");
    // A renamed bind (`x: a`) binds the value under the new name.
    assert_eq!(jsc(&["-p", "let {x:a} = {x:5}; a"]), "5");
    // A default fills a missing (undefined) key.
    assert_eq!(jsc(&["-p", "let {z=7} = {}; z"]), "7");
    // A rest collects every remaining own key into a new object.
    assert_eq!(
        jsc(&["-p", "let {a, ...rest} = {a:1,b:2,c:3}; rest.b"]),
        "2"
    );
    // The rest object excludes the destructured keys.
    assert_eq!(
        jsc(&["-p", "let {a, ...rest} = {a:1,b:2,c:3}; rest.a"]),
        "undefined"
    );
    // A nested object pattern recurses into a sub-object.
    assert_eq!(jsc(&["-p", "let {p:{q}} = {p:{q:9}}; q"]), "9");
}

/// Destructuring ASSIGNMENT to member-expression targets (`o.p`, `o[k]`,
/// `a[i]`) in object and array patterns, with defaults, nesting and rest.
#[test]
fn destructuring_assignment_to_member_targets() {
    assert_eq!(
        jsc(&[
            "-e",
            "const o={q:0,a:0,b:0,x:0}; ({a: o.q} = {a: 1}); [o.a, o.b] = [1, 2]; \
             for ({x: o.x} of [{x:5}]); console.log(o.q,o.a,o.b,o.x)",
        ]),
        "1 1 2 5"
    );
    // The RHS is evaluated before any target is written.
    assert_eq!(
        jsc(&[
            "-e",
            "const a=[0,0]; [a[1], a[0]] = [a[0]+1, 2]; console.log(a)"
        ]),
        "[ 2, 1 ]"
    );
    // A default fills an `undefined` source property.
    assert_eq!(
        jsc(&["-e", "const o={q:0}; ({q: o.q = 9} = {}); console.log(o.q)"]),
        "9"
    );
    // Computed keys, `this`-style receivers inside methods, nesting and rest.
    assert_eq!(
        jsc(&[
            "-e",
            "const o={p:0,r:0}; const k='p'; [o[k], ...o.r] = [1, 2, 3]; console.log(o.p, o.r)",
        ]),
        "1 [ 2, 3 ]"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={a:0,b:0}; ({x: [o.a, {y: o.b}]} = {x: [3, {y: 4}]}); console.log(o.a + o.b)",
        ]),
        "7"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={v:0, set(p){ [this.v] = p; }}; o.set([8]); console.log(o.v)",
        ]),
        "8"
    );
    // Identifier and member targets mix in one pattern.
    assert_eq!(
        jsc(&[
            "-e",
            "let x=0; const o={y:0}; ({x, y: o.y} = {x:1, y:2}); console.log(x, o.y)"
        ]),
        "1 2"
    );
}

/// Each member target's reference (object, then key) is evaluated before the
/// value is fetched from the source, left to right.
#[test]
fn destructuring_assignment_member_target_evaluation_order() {
    assert_eq!(
        jsc(&[
            "-e",
            "let log=''; const o={a:0,b:0}; \
             function t(n){ log += n; return o; } function k(n){ log += n; return n; } \
             [t('1')[k('a')], t('2')[k('b')]] = [1, 2]; console.log(log, o.a, o.b)",
        ]),
        "1a2b 1 2"
    );
    // The source property getter runs after the target reference.
    assert_eq!(
        jsc(&[
            "-e",
            "let log=''; const o={a:0}; class S { get a(){ log += 'G'; return 5; } } \
             function t(){ log += 'T'; return o; } ({a: t().a} = new S()); console.log(log, o.a)",
        ]),
        "TG 5"
    );
}

/// A bare member for-of head assigns each element to the member.
#[test]
fn for_of_member_and_pattern_assignment_heads() {
    assert_eq!(
        jsc(&[
            "-e",
            "const o={x:0}; for (o.x of [1,2,3]); console.log(o.x)"
        ]),
        "3"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={a:0,b:0}; let s=0; for ([o.a, o.b] of [[1,2],[3,4]]) s += o.a*o.b; \
             console.log(s, o.a, o.b)",
        ]),
        "14 3 4"
    );
}

/// A destructuring assignment's value is its right-hand side.
#[test]
fn destructuring_assignment_completion_is_rhs() {
    assert_eq!(jsc(&["-p", "const o={a:0}; [o.a] = [4]"]), "[ 4 ]");
    // Inside a top-level `try`, the RHS is the tracked completion value.
    assert_eq!(
        jsc(&["-p", "const o={a:0}; try { [o.a] = [4] } catch (e) {}"]),
        "[ 4 ]"
    );
}

/// A destructuring assignment to an absent member adds the property.
#[test]
fn destructuring_assignment_unknown_member_adds_property() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "const o={a:0}; [o.zz] = [1]; console.log(o)"])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "{ a: 0, zz: 1 }"
    );
}

/// An empty object pattern still requires an object-coercible value:
/// destructuring `null`/`undefined` throws a `TypeError`.
#[test]
fn empty_object_pattern_rejects_nullish() {
    assert_eq!(
        jsc(&[
            "-e",
            "let f = ({}) => 1; \
             try { f(null) } catch (e) { console.log(e.name) } \
             function g({} = undefined) { return 2 } \
             try { g() } catch (e) { console.log(e.name) } \
             console.log(f({}), g(0))"
        ]),
        "TypeError\nTypeError\n1 2"
    );
}

/// A logging iterable for the protocol tests below: `next` yields 1, 2, ...
/// (`undefined` each time with `blank`; endless unless `limit`), and every
/// `next` / `return` call is logged.
const LOGGED: &str = "const log = []; \
     function logged(limit, blank) { let n = 0; return { [Symbol.iterator]() { return { \
       next() { n++; log.push('next'); return { value: blank ? undefined : n, done: limit !== undefined && n > limit } }, \
       return() { log.push('return'); return {} } } } } }; ";

/// B4: an empty pattern never steps a generator (its body never runs) and
/// closes it; an elision steps once; an element steps once and closes.
#[test]
fn array_pattern_steps_lazily_and_closes_generators() {
    let program = "let ran = 0; \
         function* g() { ran++; yield 1; ran++; yield 2; ran++ } \
         let it = g(); let [] = it; console.log(ran, it.next().done); \
         it = g(); [] = it; console.log(ran, it.next().done); \
         it = g(); [,] = it; console.log(ran, it.next().done); \
         it = g(); let [a] = it; console.log(ran, a, it.next().done); \
         for (let [] of [g()]) console.log(ran)";
    assert_eq!(jsc(&["-e", program]), "0 true\n0 true\n1 true\n2 1 true\n2");
}

/// An element against a protocol iterator steps once, then calls `return`
/// exactly once; elisions step without closing early; a done iterator is
/// not closed.
#[test]
fn array_pattern_closes_protocol_iterator_once() {
    let program = format!(
        "{LOGGED} \
         let [a] = logged(); console.log(a, log.join()); log.length = 0; \
         [,] = logged(); console.log(log.join()); log.length = 0; \
         [] = logged(); console.log(log.join()); log.length = 0; \
         let [b, c, d] = logged(1); console.log(b, c, d, log.join())"
    );
    assert_eq!(
        jsc(&["-e", &program]),
        "1 next,return\nnext,return\nreturn\n1 undefined undefined next,next"
    );
}

/// An element pattern over an endless iterator terminates (no drain).
#[test]
fn array_pattern_over_infinite_iterator_terminates() {
    let program = "function* infinite() { let i = 0; while (true) yield i++ } \
         const [a, b] = infinite(); let x, y; [x, , y] = infinite(); \
         console.log(a, b, x, y)";
    assert_eq!(jsc(&["-e", program]), "0 1 0 2");
}

/// A throwing default initializer closes the iterator before the throw
/// propagates; a throw from `next` itself does not close it.
#[test]
fn array_pattern_default_throw_closes() {
    let program = format!(
        "{LOGGED} \
         try {{ let [a, b = (() => {{ throw new Error('boom') }})()] = logged(undefined, true) }} \
         catch (e) {{ console.log(e.message, log.join()) }} log.length = 0; \
         let o = {{}}; \
         try {{ [o.a, o.b = (() => {{ throw 1 }})()] = logged(undefined, true) }} \
         catch (e) {{ console.log(e, log.join()) }} log.length = 0; \
         const bad = {{ [Symbol.iterator]() {{ return {{ \
           next() {{ throw 2 }}, return() {{ log.push('return'); return {{}} }} }} }} }}; \
         try {{ let [z = 0] = bad }} catch (e) {{ console.log(e, log.length) }}"
    );
    assert_eq!(
        jsc(&["-e", &program]),
        "boom next,next,return\n1 next,next,return\n2 0"
    );
}

/// A rest element drains the iterator and does not close it.
#[test]
fn array_rest_drains_without_close() {
    let program = format!(
        "{LOGGED} \
         let [a, ...r] = logged(3); console.log(a, r.join(), log.join()); log.length = 0; \
         let s; [, ...s] = logged(2); console.log(s.join(), log.join())"
    );
    assert_eq!(
        jsc(&["-e", &program]),
        "1 2,3 next,next,next,next\n2 next,next,next"
    );
}

/// `next` calls and default evaluations interleave in element order.
#[test]
fn array_pattern_interleaves_steps_and_defaults() {
    let program = format!(
        "{LOGGED} \
         function f() {{ log.push('f'); return 'F' }} \
         function h() {{ log.push('h'); return 'H' }} \
         let [a = f(), b = h()] = logged(undefined, true); console.log(a, b, log.join()); log.length = 0; \
         let x, y; [x = f(), y = h()] = logged(undefined, true); console.log(x, y, log.join())"
    );
    assert_eq!(
        jsc(&["-e", &program]),
        "F H next,f,next,h,return\nF H next,f,next,h,return"
    );
}

/// A for-of head pattern closes each element's iterator; a parameter
/// pattern closes its argument's iterator.
#[test]
fn for_of_heads_and_parameters_close() {
    let program = format!(
        "{LOGGED} \
         for (const [a] of [logged(), logged()]) log.push(a); console.log(log.join()); \
         log.length = 0; \
         function p([a]) {{ return a }} console.log(p(logged()), log.join()); log.length = 0; \
         const q = ([a, b]) => a + b; console.log(q(logged()), log.join())"
    );
    assert_eq!(
        jsc(&["-e", &program]),
        "next,return,1,next,return,1\n1 next,return\n3 next,next,return"
    );
}
