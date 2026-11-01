//! Integration tests: labels on any statement (DESIGN.md §7.3) and the
//! remaining assignment / update / `delete` targets (DESIGN.md §7.2) —
//! labelled blocks, `if`, `try`, `switch` and nested labels, `break` through
//! `finally`, labelled jumps in generator bodies, the label early errors,
//! `**=` / logical assignment to members with single evaluation and a skipped
//! store, parenthesized targets, invalid targets, and `delete` of a
//! non-reference.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_bool, assert_number, assert_output, assert_rejected, assert_string};

/// Assert `src` fails to compile with a message containing `msg`.
fn assert_compile_error(src: &str, msg: &str) {
    let err = jsc_core::run::compile_and_run(src).expect_err("program should be rejected");
    let text = err.to_string();
    assert!(
        text.contains(msg),
        "expected `{msg}` for `{src}`, got `{text}`"
    );
}

#[test]
fn break_exits_a_labelled_block() {
    assert_string(
        "let s = ''; a: { s += 'x'; if (s) break a; s += 'y'; } s += 'z'; s",
        "xz",
    );
    // The label is still a normal statement when nothing breaks.
    assert_string("let s = ''; a: { s += 'x'; } s", "x");
    // An empty labelled statement and a labelled expression statement.
    assert_number("let n = 1; a: ; b: n += 1; n", 2.0);
}

#[test]
fn break_exits_labelled_if_try_and_switch() {
    assert_string(
        "let s = ''; a: if (true) { s += '1'; break a; } else { s += '2'; } s",
        "1",
    );
    assert_string(
        "let s = ''; a: try { s += 't'; break a; } catch (e) { s += 'c'; } s += '.'; s",
        "t.",
    );
    // `break a` from a switch case exits the labelled switch; an unlabeled
    // `break` still exits only the switch.
    assert_string(
        "let s = ''; a: switch (1) { case 1: s += '1'; break a; case 2: s += '2'; } s",
        "1",
    );
    assert_string(
        "let s = ''; a: { switch (1) { case 1: s += '1'; break; } s += '2'; break a; } s",
        "12",
    );
}

#[test]
fn break_through_finally_runs_it_in_order() {
    assert_output(
        "a: { try { try { console.log('body'); break a; } finally { console.log('inner'); } } \
         finally { console.log('outer'); } console.log('skipped'); } console.log('after');",
        "body\ninner\nouter\nafter\n",
    );
    // A `finally` that itself breaks to a label overrides a pending throw.
    assert_string(
        "let s = ''; a: { try { throw new Error('x'); } finally { s += 'f'; break a; } } s",
        "f",
    );
}

#[test]
fn break_to_an_outer_labelled_block_from_loops() {
    assert_string(
        "let s = ''; a: { for (let i = 0; i < 5; i++) { while (true) { s += i; if (i == 2) break a; break; } } \
         s += 'end'; } s",
        "012",
    );
    // An unlabeled `break` inside a labelled block in a loop exits the loop.
    assert_string(
        "let s = ''; for (let i = 0; i < 5; i++) { a: { if (i == 2) break; s += i; } } s",
        "01",
    );
    // `continue` inside a labelled block targets the enclosing loop.
    assert_string(
        "let s = ''; for (let i = 0; i < 4; i++) { a: { if (i % 2) continue; s += i; } s += '.'; } s",
        "0.2.",
    );
}

#[test]
fn nested_labels_name_one_statement() {
    assert_string(
        "let s = ''; a: b: for (let i = 0; i < 4; i++) { if (i == 1) continue a; if (i == 3) break b; s += i; } s",
        "02",
    );
    assert_string(
        "let s = ''; a: b: { s += '1'; break a; } c: d: { s += '2'; break d; } s",
        "12",
    );
    assert_string(
        "let s = ''; outer: for (const x of [1, 2]) { inner: { if (x == 1) break inner; continue outer; } s += x; } s",
        "1",
    );
}

#[test]
fn labelled_statements_in_functions_and_closures() {
    assert_number(
        "function f(n) { let r = 0; done: { if (n > 1) { r = 1; break done; } r = 2; } return r; } f(5) * 10 + f(0)",
        12.0,
    );
    // Labels do not cross a function boundary: the same name is reusable.
    assert_number(
        "function g() { a: { return 1; } } a: { g(); break a; } g()",
        1.0,
    );
}

#[test]
fn labelled_break_in_a_generator() {
    assert_string(
        "function* g() { a: { yield 1; if (true) break a; yield 2; } yield 3; } [...g()].join()",
        "1,3",
    );
    assert_string(
        "function* g() { a: try { yield 1; break a; } finally { yield 'f'; } yield 'end'; } [...g()].join()",
        "1,f,end",
    );
    assert_string(
        "function* g() { for (const x of [1, 2, 3]) { a: { if (x == 2) break a; yield x; } } } [...g()].join()",
        "1,3",
    );
    assert_string(
        "function* g() { a: b: for (let i = 0; i < 3; i++) { yield i; if (i == 1) break a; } } [...g()].join()",
        "0,1",
    );
}

#[test]
fn labelled_break_in_an_async_function() {
    assert_output(
        "async function f() { a: { await null; console.log('in'); break a; } console.log('out'); } f();",
        "in\nout\n",
    );
}

#[test]
fn label_early_errors() {
    // `continue` to a label that does not denote an iteration statement.
    assert_compile_error(
        "a: { while (true) { continue a; } }",
        "does not denote an iteration statement",
    );
    assert_compile_error(
        "a: if (true) { for (;;) { continue a; } }",
        "does not denote an iteration statement",
    );
    // Undefined and duplicate labels, a labelled function declaration.
    assert_rejected("a: { break b; }");
    assert_rejected("while (true) { continue b; }");
    assert_rejected("a: a: ;");
    assert_rejected("a: { a: { } }");
    assert_rejected("a: function f() {}");
    // An unlabeled `break` / `continue` never targets a labelled block.
    assert_rejected("a: { break; }");
    assert_rejected("a: { continue; }");
}

/// An object whose `x` accessor logs every get / set into `log`.
const LOGGED: &str = "const log = []; let key = 0;
     const o = { v: 2,
       get x() { log.push('get'); return this.v },
       set x(n) { log.push('set ' + n); this.v = n } };
     const base = () => { log.push('base'); return o };
     const k = () => { log.push('key'); key++; return 'x' };";

#[test]
fn pow_assign_on_members() {
    assert_number("const o = { x: 3 }; o.x **= 2; o.x", 9.0);
    assert_number("const o = { x: 3 }; o['x'] **= 3", 27.0);
    assert_number("const a = [2, 3]; a[1] **= 2; a[0] + a[1]", 11.0);
    assert_string(
        &format!("{LOGGED} const r = base()[k()] **= 3; log.push(r); log.join()"),
        "base,key,get,set 8,8",
    );
    assert_number("let a = 2; (a) **= 3; a", 8.0);
    assert_rejected("const o = null; o.x **= 2");
}

#[test]
fn logical_assign_on_members_evaluates_once_and_skips_the_store() {
    // Short-circuited: the right-hand side and the setter never run.
    assert_string(
        &format!(
            "{LOGGED} const r = base()[k()] ??= (log.push('rhs'), 9); log.push(r); log.join()"
        ),
        "base,key,get,2",
    );
    assert_string(
        &format!(
            "{LOGGED} const r = base()[k()] ||= (log.push('rhs'), 9); log.push(r); log.join()"
        ),
        "base,key,get,2",
    );
    // Not short-circuited: the right-hand side runs, then the setter, once.
    assert_string(
        &format!(
            "{LOGGED} const r = base()[k()] &&= (log.push('rhs'), 9); log.push(r); log.join()"
        ),
        "base,key,get,rhs,set 9,9",
    );
    assert_string(
        &format!("{LOGGED} o.v = null; o.x ??= 5; log.join()"),
        "get,set 5",
    );
    assert_string(
        &format!("{LOGGED} o.v = 0; o.x ||= 7; log.join() + ' ' + o.v"),
        "get,set 7 7",
    );
    assert_number("const o = {}; o.a ??= 1; o.a ??= 2; o.a", 1.0);
    assert_number(
        "const a = [0]; a[0] ||= 4; a[1] &&= 5; a[0] + a.length",
        5.0,
    );
    // The key is converted once.
    assert_string(
        "let n = 0; const key = { toString() { n++; return 'p' } }; const o = {}; o[key] ??= 1; o[key] ||= 2; n + ':' + o.p",
        "2:1",
    );
}

#[test]
fn logical_assign_names_anonymous_functions() {
    assert_string("let f; f ??= function () {}; f.name", "f");
    assert_string("let g = 0; g ||= () => 1; g.name", "g");
    assert_string("let h = 1; h &&= class {}; h.name", "h");
    // A parenthesized identifier is not an IdentifierRef: no name.
    assert_string("let f; (f) = function () {}; f.name", "");
    assert_string("let f; (f) ??= function () {}; f.name", "");
}

#[test]
fn logical_assign_to_identifiers_skips_the_write() {
    assert_number("let a = 1; a ||= 2; a", 1.0);
    assert_number("let a = null; a ??= 3; a", 3.0);
    assert_number("let a = 1; (a) &&= 4; a", 4.0);
    assert_string(
        "let log = ''; let a = 1; const r = a ||= (log += 'rhs', 2); log + r",
        "1",
    );
}

#[test]
fn logical_and_pow_assign_in_a_generator() {
    assert_string(
        "function* g() { const o = { a: null, b: 2 }; o.a ??= yield 'x'; o.b **= yield 'y'; return o.a + ',' + o.b; } \
         const it = g(); it.next(); it.next(5); it.next(3).value",
        "5,8",
    );
    assert_string(
        "function* g() { const o = { a: 1 }; o.a ||= yield 'never'; yield o.a; } [...g()].join()",
        "1",
    );
}

#[test]
fn parenthesized_targets() {
    assert_number("let a; (a) = 1; a", 1.0);
    assert_number("let a; ((a)) = 2; a", 2.0);
    assert_number("let a = 1; (a) += 4; a", 5.0);
    assert_number("const o = { x: 1 }; (o.x) += 1; o.x", 2.0);
    assert_number("const o = { x: 1 }; ((o['x'])) = 7; o.x", 7.0);
    assert_number("let a = 1; (a)++; ++(a); ((a))--; a", 2.0);
    assert_number("const o = { x: 1 }; (o.x)++; ++((o.x)); o.x", 3.0);
    assert_number("let a = 5; const r = (a)--; r * 10 + a", 54.0);
}

#[test]
fn invalid_assignment_targets_are_syntax_errors() {
    assert_rejected("function f() {} f() = 1");
    assert_rejected("1 = 2");
    assert_rejected("let a, b, c; a + b = c");
    assert_rejected("let a, b, c; (a, b) = c");
    assert_rejected("let a = {}; a?.b = 1");
    assert_rejected("let a = {}; a?.b ??= 1");
    assert_rejected("function f() {} f() ??= 1");
    assert_rejected("function f() {} f()++");
    assert_rejected("let a = 1; ++(a + 1)");
    assert_rejected("let a = 1, b = 2; (a, b) **= 2");
}

#[test]
fn delete_of_a_non_reference() {
    assert_bool("delete 1", true);
    assert_string(
        "let n = 0; function f() { n++; return 1 } const r = delete f(); r + ':' + n",
        "true:1",
    );
    assert_bool("const o = { a: 1 }; delete (o.a) && !('a' in o)", true);
    assert_bool("delete (1 + 2)", true);
    // `delete x` of an identifier is a strict-mode early error.
    assert_compile_error("let x = 1; delete x", "Delete of an unqualified identifier");
    assert_rejected("let x = 1; delete (x)");
}
