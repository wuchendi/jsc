//! Builds the program text handed to jsc.
//!
//! [`Mode::Strict`] programs are `"use strict";`, then the harness `sta.js`,
//! `assert.js` and the listed includes, then the test source. [`Mode::Raw`]
//! programs are the test source byte-for-byte.
//!
//! # Adapter rewrites
//!
//! jsc functions cannot carry properties, so the harness shims define
//! `assert_sameValue`, `assert_throws`, ... as plain functions. Two rewrites
//! are applied to strict test sources, in this order:
//!
//! 1. `assert.throws(<Name>,` becomes `assert_throws_<Name>(` for `<Name>` in
//!    [`THROWS_SHIMMED`] (whitespace is allowed after `(` and before `,`).
//!    jsc cannot yet compare `.constructor` identity, so the shims check these
//!    constructors by name; any other constructor falls through to rewrite 2
//!    and the generic `assert_throws`, which fails honestly.
//! 2. Every `assert.<ident>(` becomes `assert_<ident>(`, where `<ident>` is
//!    `[A-Za-z_$][A-Za-z0-9_$]*`.
//!
//! In both, `assert` must not be preceded by an identifier character or `.`.
//! The scans are purely lexical (strings and comments are rewritten too);
//! includes are never rewritten.

use crate::filter::Mode;

/// Assembles the program for `source` run in `mode`.
pub fn program(mode: Mode, source: &str, sta: &str, assert: &str, includes: &[&str]) -> String {
    match mode {
        Mode::Raw => source.to_owned(),
        Mode::Strict => {
            let mut out = String::from("\"use strict\";\n");
            for part in [sta, assert].iter().chain(includes) {
                out.push_str(part);
                out.push('\n');
            }
            out.push_str(&rewrite(&rewrite_throws(source)));
            out
        }
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Constructors with a dedicated `assert_throws_<Name>` harness shim.
pub const THROWS_SHIMMED: [&str; 4] = ["Test262Error", "Error", "TypeError", "RangeError"];

/// Whether `assert` starting at byte `at` is not part of a longer name or
/// member expression.
fn standalone(bytes: &[u8], at: usize) -> bool {
    at == 0 || !(is_ident(bytes[at - 1]) || bytes[at - 1] == b'.')
}

/// Applies adapter rewrite 1 (`assert.throws(<Name>,`) from the module docs.
pub(crate) fn rewrite_throws(source: &str) -> String {
    const CALL: &str = "assert.throws(";
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(off) = source[from..].find(CALL) {
        let at = from + off;
        from = at + 1;
        if !standalone(bytes, at) {
            continue;
        }
        let rest = &source[at + CALL.len()..];
        let name_rest = rest.trim_start();
        let name_len = name_rest
            .bytes()
            .position(|b| !is_ident(b))
            .unwrap_or(name_rest.len());
        let name = &name_rest[..name_len];
        let after = name_rest[name_len..].trim_start();
        if THROWS_SHIMMED.contains(&name) && after.starts_with(',') {
            out.push_str(&source[last..at]);
            out.push_str("assert_throws_");
            out.push_str(name);
            out.push('(');
            last = source.len() - after.len() + 1;
            from = last;
        }
    }
    out.push_str(&source[last..]);
    out
}

/// Applies adapter rewrite 2 (`assert.<ident>(`) from the module docs.
pub(crate) fn rewrite(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(off) = source[from..].find("assert.") {
        let at = from + off;
        from = at + 1;
        if !standalone(bytes, at) {
            continue;
        }
        let dot = at + 6;
        let name_start = dot + 1;
        let mut end = name_start;
        while end < bytes.len() && is_ident(bytes[end]) {
            end += 1;
        }
        if end > name_start && !bytes[name_start].is_ascii_digit() && bytes.get(end) == Some(&b'(')
        {
            out.push_str(&source[last..dot]);
            out.push('_');
            last = dot + 1;
        }
    }
    out.push_str(&source[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_prefix_and_order() {
        let p = program(
            Mode::Strict,
            "assert.sameValue(1, 1);",
            "STA",
            "ASSERT",
            &["INC1", "assert.x("],
        );
        assert_eq!(
            p,
            "\"use strict\";\nSTA\nASSERT\nINC1\nassert.x(\nassert_sameValue(1, 1);"
        );
    }

    #[test]
    fn raw_untouched() {
        let src = "assert.sameValue(1, 1); 'use strict'";
        assert_eq!(program(Mode::Raw, src, "STA", "ASSERT", &["I"]), src);
    }

    #[test]
    fn rewrite_cases() {
        assert_eq!(rewrite("assert(x);"), "assert(x);");
        assert_eq!(
            rewrite("assert.sameValue(a, b);"),
            "assert_sameValue(a, b);"
        );
        assert_eq!(rewrite("x;assert.throws(E, f)"), "x;assert_throws(E, f)");
        assert_eq!(rewrite("assert.$_9("), "assert_$_9(");
        assert_eq!(rewrite("myassert.x(1)"), "myassert.x(1)");
        assert_eq!(rewrite("foo.assert.x(1)"), "foo.assert.x(1)");
        assert_eq!(
            rewrite("assert.x;assert.(;assert.1("),
            "assert.x;assert.(;assert.1("
        );
        assert_eq!(rewrite("é assert.ok("), "é assert_ok(");
    }

    #[test]
    fn throws_rewrite_cases() {
        assert_eq!(
            rewrite_throws("assert.throws(TypeError, f)"),
            "assert_throws_TypeError( f)"
        );
        assert_eq!(
            rewrite_throws("assert.throws( \n RangeError \t,() => {})"),
            "assert_throws_RangeError(() => {})"
        );
        assert_eq!(
            rewrite_throws("assert.throws(Test262Error,f);assert.throws(Error,g)"),
            "assert_throws_Test262Error(f);assert_throws_Error(g)"
        );
        for kept in [
            "assert.throws(SyntaxError, f)",
            "assert.throws(ErrorX, f)",
            "assert.throws(Error.x, f)",
            "assert.throws(TypeError)",
            "myassert.throws(Error, f)",
            "x.assert.throws(Error, f)",
            "assert.throwsX(Error, f)",
        ] {
            assert_eq!(rewrite_throws(kept), kept);
        }
    }

    #[test]
    fn throws_rewrite_precedes_generic() {
        let p = program(
            Mode::Strict,
            "assert.throws(TypeError, f);assert.throws(SyntaxError, g);",
            "S",
            "A",
            &[],
        );
        assert!(
            p.ends_with("assert_throws_TypeError( f);assert_throws(SyntaxError, g);"),
            "{p}"
        );
    }
}
