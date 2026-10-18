//! Builds the program text handed to jsc.
//!
//! [`Mode::Strict`] programs are `"use strict";`, then the harness `sta.js`,
//! `assert.js` and the listed includes, then the test source. [`Mode::Raw`]
//! programs are the test source byte-for-byte.
//!
//! # Adapter rewrite
//!
//! jsc functions cannot carry properties, so the harness shims define
//! `assert_sameValue`, `assert_throws`, ... as plain functions. The single
//! rewrite applied to strict test sources turns every `assert.<ident>(` into
//! `assert_<ident>(`, where `<ident>` is `[A-Za-z_$][A-Za-z0-9_$]*` and
//! `assert` is not preceded by an identifier character or `.`. The scan is
//! purely lexical (strings and comments are rewritten too); includes are
//! never rewritten.

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
            out.push_str(&rewrite(source));
            out
        }
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Applies the adapter rewrite described in the module docs.
pub(crate) fn rewrite(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(off) = source[from..].find("assert.") {
        let at = from + off;
        from = at + 1;
        if at > 0 && (is_ident(bytes[at - 1]) || bytes[at - 1] == b'.') {
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
}
