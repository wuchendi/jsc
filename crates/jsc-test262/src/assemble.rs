//! Builds the program text handed to jsc.
//!
//! [`Mode::Strict`] programs are `"use strict";`, then the official harness
//! `sta.js`, `assert.js` and the listed includes, then the test source, all
//! unmodified. [`Mode::Raw`] programs are the test source byte-for-byte.

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
            out.push_str(source);
            out
        }
    }
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
            &["INC1", "INC2"],
        );
        assert_eq!(
            p,
            "\"use strict\";\nSTA\nASSERT\nINC1\nINC2\nassert.sameValue(1, 1);"
        );
    }

    #[test]
    fn raw_untouched() {
        let src = "assert.sameValue(1, 1); 'use strict'";
        assert_eq!(program(Mode::Raw, src, "STA", "ASSERT", &["I"]), src);
    }
}
