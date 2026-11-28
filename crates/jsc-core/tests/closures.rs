//! Integration tests: closure capture semantics (boxed cells, block scope, loops).

mod common;
use common::{assert_eval, assert_output, assert_same_under_gc_stress, assert_uncaught};

/// 4. A closure that **captures a block-scoped** binding declared inside a
///    nested block, read after the block via the returned function.
#[test]
fn closure_captures_block_scoped_binding() {
    assert_eval(
        "function make() {
           let f = () => 0;
           { let secret = 42; f = () => secret; }
           return f;
         }
         make()()",
        "42",
    );
}

/// 5. An **arrow inside a function inside an arrow** — three nesting levels,
///    with the innermost capturing bindings from every enclosing scope.
#[test]
fn arrow_inside_function_inside_arrow() {
    assert_eval(
        "let outer = a => {
           function middle(b) {
             let inner = c => a + b + c;
             return inner;
           }
           return middle;
         };
         outer(30)(10)(2)",
        "42",
    );
}

/// 6. A **closure counter** whose captured cell is mutated across repeated
///    calls driven by a `while` loop, proving shared mutable state persists.
#[test]
fn closure_counter_across_loop() {
    assert_eval(
        "function counter() { let c = 0; return function() { c = c + 1; return c } }
         let n = counter();
         let last = 0;
         let i = 0;
         while (i < 42) { last = n(); i = i + 1 }
         last",
        "42",
    );
}

/// 8. Two closures over the **same** cell (accumulator pattern) — mutation
///    through one call is visible to the next; a fresh accumulator is
///    independent.
#[test]
fn accumulator_shares_and_isolates_cells() {
    assert_eval(
        "function acc() { let s = 0; return x => { s += x; return s } }
         let a = acc();
         a(10);
         a(32)",
        "42",
    );
    // A second accumulator starts from its own independent cell.
    assert_eval(
        "function acc() { let s = 0; return x => { s += x; return s } }
         let a = acc();
         let b = acc();
         a(100);
         b(42)",
        "42",
    );
}

/// 9. Top-level bindings referenced by top-level functions live in module cells:
///    the functions stay hoisted, `let`/`const`/`class` keep their TDZ (an access
///    before the declaration throws a `ReferenceError`), a `var` reads
///    `undefined` before its declaration, and a declaration never becomes the
///    program's completion value.
#[test]
fn module_bindings_keep_tdz_and_hoisting() {
    assert_uncaught(
        "f(); let n = 41; function f() { return n }",
        "ReferenceError: Cannot access 'n' before initialization",
    );
    assert_uncaught(
        "f(); let n = 41; function f() { n = 1 }",
        "ReferenceError: Cannot access 'n' before initialization",
    );
    assert_eval(
        "function f() { n = 3 } \
         let r = 0; try { f() } catch (e) { r = e instanceof Error } \
         let n = 1; r",
        "true",
    );
    assert_eval("let n = 41; function f() { return n } f()", "41");
    assert_eval("f(); var n = 41; function f() { return n }", "undefined");
    assert_eval("let k = 5; function a() { return k }", "undefined");
    assert_eval("class Q {} function a() { return Q }", "undefined");
}

/// 10. Module bindings are reached from every function shape nested in a
///     top-level function: generators, async bodies, nested arrows, class
///     methods/static fields, and expression forms like template literals and
///     optional chains.
#[test]
fn module_bindings_reachable_from_nested_shapes() {
    assert_eval(
        "function* g() { yield k; yield k + 1 } let k = 10; \
         let it = g(); it.next().value + it.next().value",
        "21",
    );
    assert_output(
        "let k = 7; async function f() { await null; return k } \
         f().then(v => console.log(v))",
        "7\n",
    );
    assert_eval("let k = 5; function f() { return () => k } f()()", "5");
    assert_eval(
        "let k = 5; class A { m() { return k } } function f() { return new A().m() } f()",
        "5",
    );
    assert_eval(
        "let k = 5; function f() { class B { static s = k } return B.s } f()",
        "5",
    );
    assert_eval("let k = 1; function f() { return `v${k}` } f()", "v1");
    assert_eval(
        "let o = {a: {b: 2}}; function f() { return o?.a?.b } f()",
        "2",
    );
    assert_eval(
        "let i = 0; function f() { do { i++ } while (i < 3); return i } f()",
        "3",
    );
}

/// A computed object-literal key inside a function that references an outer
/// binding is captured like any other reference.
#[test]
fn computed_object_key_captures_outer_binding() {
    assert_output(
        "let k = 'a'; function f() { return {[k]: 1} } console.log(f())",
        "{ a: 1 }\n",
    );
    assert_output(
        "function outer() { let k = 'm'; return () => ({ [k]() { return 2 }, get [k + 'g']() { return 3 } }) } \
         let o = outer()(); console.log(o.m(), o.mg)",
        "2 3\n",
    );
}

/// A captured binding initialized with a non-empty array or object literal
/// holds that value: the literal's build must not clobber the cell it is
/// stored into.
#[test]
fn captured_binding_holds_heap_literal() {
    assert_output(
        "let a = [7]; const f = () => a; console.log(f(), a[0])",
        "[ 7 ] 7\n",
    );
    assert_output(
        "function m() { let o = {x: 1}; const g = () => o.x; return g() } console.log(m())",
        "1\n",
    );
}

/// A captured binding initialized with a freshly allocated object or array
/// keeps it: building the value must not clobber its cell.
#[test]
fn captured_heap_value_initializer() {
    assert_output(
        "let o = {tag: 'o'}; let a = [5]; let f = () => [o.tag, a[0]]; console.log(f())",
        "[ o, 5 ]\n",
    );
    assert_eval(
        "function g(){ let o = {a: 1}; return () => o.a } g()()",
        "1",
    );
}

/// A closure created in a block-scoped binding's own initializer (an object
/// method, an arrow, a getter) captures that binding: called after the
/// declaration it sees the initialized value, not an unresolvable global.
/// Covers function bodies, `try`/`catch`/`finally` blocks, plain blocks
/// (top level too), `let` and `const`.
#[test]
fn closure_in_own_initializer_captures_binding() {
    assert_eval(
        "(function () { try { const h = { m() { return h; } }; return h.m() === h; } \
         catch (e) { return String(e); } })()",
        "true",
    );
    assert_eval(
        "(function () { let h = { m() { return h; } }; return h.m() === h; })()",
        "true",
    );
    assert_eval(
        "function f() { const fact = (n) => n ? n * fact(n - 1) : 1; return fact(5) } f()",
        "120",
    );
    assert_eval(
        "function f() { const o = { get self() { return o } }; return o.self === o } f()",
        "true",
    );
    assert_eval("{ const g = () => g; g() === g }", "true");
    assert_eval(
        "function f() { let r; \
           try { throw 1 } catch (e) { const k = () => k; r = k() === k } \
           finally { const z = { m() { return z } }; r = r && z.m() === z } \
           return r } f()",
        "true",
    );
    assert_eval(
        "function* g() { const o = { m() { return o } }; yield o.m() === o } g().next().value",
        "true",
    );
    assert_eval(
        "function f() { const [h = () => h] = []; return h() === h } f()",
        "true",
    );
}

/// The same capture under `gc_stress = 1`, with allocations between the
/// declaration and the call.
#[test]
fn closure_in_own_initializer_survives_gc_stress() {
    assert_same_under_gc_stress(
        "(function () { const h = { m() { return h; } }; const a = []; \
           for (let i = 0; i < 50; i++) a.push({ i }); \
           console.log(h.m() === h, a.length) })()",
        "true 50\n",
    );
}

/// A block's `let`/`const`/`class` bindings exist from the start of the
/// block: a closure created by an earlier statement resolves to the later
/// declaration (shadowing an outer binding of the same name), reads it once
/// initialized, and throws a `ReferenceError` while it is in its TDZ.
#[test]
fn closure_before_declaration_resolves_block_binding() {
    assert_eval(
        "function f() { const a = () => b; const b = 2; return a() } f()",
        "2",
    );
    assert_output(
        "function f() { let x = 'outer'; { const g = () => x; let x = 'inner'; return g() } } \
         console.log(f())",
        "inner\n",
    );
    assert_eval(
        "function f() { const mk = () => new C(); class C {} return mk() instanceof C } f()",
        "true",
    );
    assert_uncaught(
        "(function () { const g = () => x; g(); let x = 1; })()",
        "ReferenceError: Cannot access 'x' before initialization",
    );
    assert_eval(
        "function f() { const g = () => x; try { g() } catch (e) { return e instanceof ReferenceError } \
           let x = 1 } f()",
        "true",
    );
}

/// A hoisted function declaration reading a `let` of its block: called
/// before the declaration it throws (TDZ), after it reads the value.
#[test]
fn hoisted_function_reads_block_binding_tdz() {
    assert_eval(
        "function f() { try { g() } catch (e) { return e instanceof ReferenceError } \
           let x = 1; function g() { return x } } f()",
        "true",
    );
    assert_eval(
        "function f() { let x = 1; return g(); function g() { return x } } f()",
        "1",
    );
}

/// A reference in the same function before a block-scoped declaration (its
/// own initializer included) throws instead of reading an outer binding.
#[test]
fn same_frame_read_before_declaration_throws() {
    assert_output(
        "function f() { let x = 1; { try { x } catch (e) { return String(e) } let x = 2 } } \
         console.log(f())",
        "ReferenceError: Cannot access 'x' before initialization\n",
    );
    assert_uncaught(
        "let x = 1; function f() { const x = x + 1; return x } f()",
        "ReferenceError: Cannot access 'x' before initialization",
    );
}

/// Declarations whose initializer creates a closure referring to the binding
/// being declared, one per form: an object method, an arrow, a function
/// expression, a class expression's static method, static field arrow and
/// instance field initializer, and a class declaration's static method —
/// over `let`, `const` and `class`. Each closure runs after initialization.
const SELF_REFERENCING_DECLS: &str = "
    const a = { m() { return a.v }, v: 1 };
    let b = () => typeof b;
    const c = function () { return typeof c };
    let d = class { static v = 4; static m() { return d.v } };
    const e = class { static v = 5; static s = () => e.v };
    class C { static v = 6; static m() { return C.v } }
    const g = class { static v = 7; x = g.v };
    r = [a.m(), b(), c(), d.m(), e.s(), C.m(), new g().x].join();
";

const SELF_REFERENCING_RESULT: &str = "1,function,function,4,5,6,7\n";

/// L1's reduced repro: a method in a `const` initializer inside a function.
#[test]
fn method_in_const_initializer_in_function() {
    assert_output(
        "(function(){ const h = { f(){ return h.v }, v: 3 }; console.log(h.f()) })()",
        "3\n",
    );
}

/// Every self-referencing form, in a function body, a nested block, a
/// generator body and a `while` loop body inside a function.
#[test]
fn self_referencing_initializers_in_function_scopes() {
    let d = SELF_REFERENCING_DECLS;
    for src in [
        format!("function f() {{ let r; {d} return r }} console.log(f())"),
        format!("function f() {{ let r; {{ {d} }} return r }} console.log(f())"),
        format!("function* f() {{ let r; {d} yield r }} console.log(f().next().value)"),
        format!(
            "function f() {{ let r, i = 0; while (i < 2) {{ i++; {d} }} return r }} \
             console.log(f())"
        ),
    ] {
        assert_output(&src, SELF_REFERENCING_RESULT);
    }
}

/// Every self-referencing form at the top level: directly, in a block, in
/// `try` / `catch` / `finally` blocks, and in a `for` loop body.
#[test]
fn self_referencing_initializers_in_top_level_scopes() {
    let d = SELF_REFERENCING_DECLS;
    for src in [
        format!("let r; {d} console.log(r)"),
        format!("let r; {{ {d} }} console.log(r)"),
        format!("let r; try {{ {d} }} catch (e) {{ r = String(e) }} console.log(r)"),
        format!("let r; try {{ throw 0 }} catch (_) {{ {d} }} console.log(r)"),
        format!("let r; try {{}} finally {{ {d} }} console.log(r)"),
        format!("let r; for (let i = 0; i < 2; i++) {{ {d} }} console.log(r)"),
    ] {
        assert_output(&src, SELF_REFERENCING_RESULT);
    }
}

/// The self-referencing forms under `gc_stress = 1`.
#[test]
fn self_referencing_initializers_survive_gc_stress() {
    assert_same_under_gc_stress(
        &format!("function f() {{ let r; {SELF_REFERENCING_DECLS} return r }} console.log(f())"),
        SELF_REFERENCING_RESULT,
    );
}

/// A self-referencing closure called DURING its binding's initializer sees
/// the binding in its TDZ: a method, an arrow, a function expression, and a
/// class static field initializer, in a function and in a top-level block.
#[test]
fn self_reference_during_initializer_throws_tdz() {
    let tdz = "ReferenceError: Cannot access 'h' before initialization\n";
    for init in [
        "({ m() { return h } }).m()",
        "(() => h)()",
        "(function () { return h })()",
        "class { static s = h }",
    ] {
        assert_output(
            &format!(
                "function f() {{ try {{ const h = {init}; return 'no throw' }} \
                 catch (e) {{ return String(e) }} }} console.log(f())"
            ),
            tdz,
        );
        assert_output(
            &format!(
                "try {{ let h = {init}; console.log('no throw') }} catch (e) {{ console.log(String(e)) }}"
            ),
            tdz,
        );
    }
}
