//! Builds the program text handed to jsc.
//!
//! [`Mode::Strict`] programs are `"use strict";`, then the official harness
//! `sta.js`, `assert.js` and the listed includes, then the test source, all
//! unmodified. [`Mode::Async`] programs are the same with the runner-owned
//! [`PRINT_PRELUDE`] right after the directive (`doneprintHandle.js` is passed
//! by the caller as the first include). [`Mode::Raw`] programs are the test
//! source byte-for-byte.

use crate::filter::Mode;

/// Host-defined `print` (test262 `INTERPRETING.md`), used by
/// `doneprintHandle.js` to report `$DONE`.
pub const PRINT_PRELUDE: &str = "function print(message) { console.log(message); }\n";

/// Assembles the program for `source` run in `mode`.
pub fn program(mode: Mode, source: &str, sta: &str, assert: &str, includes: &[&str]) -> String {
    match mode {
        Mode::Raw => source.to_owned(),
        Mode::Strict | Mode::Async => {
            let mut out = String::from("\"use strict\";\n");
            if mode == Mode::Async {
                out.push_str(PRINT_PRELUDE);
            }
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
    fn async_prelude_order() {
        let p = program(Mode::Async, "SRC", "STA", "ASSERT", &["DONE", "INC"]);
        assert_eq!(
            p,
            "\"use strict\";\nfunction print(message) { console.log(message); }\n\
             STA\nASSERT\nDONE\nINC\nSRC"
        );
    }

    #[test]
    fn raw_untouched() {
        let src = "assert.sameValue(1, 1); 'use strict'";
        assert_eq!(program(Mode::Raw, src, "STA", "ASSERT", &["I"]), src);
    }
}
