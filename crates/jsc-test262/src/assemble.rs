//! Builds the program text handed to jsc.
//!
//! [`Mode::Strict`] programs are `"use strict";`, then the official harness
//! `sta.js`, `assert.js` and the listed includes, then the test source, all
//! unmodified. [`Mode::Async`] programs are the same with the runner-owned
//! [`PRINT_PRELUDE`] right after the directive (`doneprintHandle.js` is passed
//! by the caller as the first include). A test that includes
//! [`DETACH_INCLUDE`] also gets the runner-owned [`DETACH_PRELUDE`] right
//! after the directive (and the print prelude). [`Mode::Raw`] programs are
//! the test source byte-for-byte.

use crate::filter::Mode;

/// Host-defined `print` (test262 `INTERPRETING.md`), used by
/// `doneprintHandle.js` to report `$DONE`.
pub const PRINT_PRELUDE: &str = "function print(message) { console.log(message); }\n";

/// The harness include that calls the host-defined
/// `$262.detachArrayBuffer`.
pub const DETACH_INCLUDE: &str = "detachArrayBuffer.js";

/// Host-defined `$262.detachArrayBuffer` (DESIGN.md §5.8.4): detaches through
/// `ArrayBuffer.prototype.transfer`, captured with the `detached` getter
/// before the test runs (so replacing them does not break the hook); an
/// already detached buffer is left alone.
pub const DETACH_PRELUDE: &str = "var $262 = (function (transfer, detached) {\n\
  return { detachArrayBuffer: function (buffer) {\n\
    if (!detached.call(buffer)) transfer.call(buffer);\n\
    return null;\n\
  } };\n\
})(ArrayBuffer.prototype.transfer,\n\
   Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, \"detached\").get);\n";

/// Assembles the program for `source` run in `mode`; `detach` adds the
/// [`DETACH_PRELUDE`] (a test including [`DETACH_INCLUDE`]).
pub fn program(
    mode: Mode,
    source: &str,
    sta: &str,
    assert: &str,
    includes: &[&str],
    detach: bool,
) -> String {
    match mode {
        Mode::Raw => source.to_owned(),
        Mode::Strict | Mode::Async => {
            let mut out = String::from("\"use strict\";\n");
            if mode == Mode::Async {
                out.push_str(PRINT_PRELUDE);
            }
            if detach {
                out.push_str(DETACH_PRELUDE);
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
            false,
        );
        assert_eq!(
            p,
            "\"use strict\";\nSTA\nASSERT\nINC1\nINC2\nassert.sameValue(1, 1);"
        );
    }

    #[test]
    fn async_prelude_order() {
        let p = program(Mode::Async, "SRC", "STA", "ASSERT", &["DONE", "INC"], false);
        assert_eq!(
            p,
            "\"use strict\";\nfunction print(message) { console.log(message); }\n\
             STA\nASSERT\nDONE\nINC\nSRC"
        );
    }

    #[test]
    fn raw_untouched() {
        let src = "assert.sameValue(1, 1); 'use strict'";
        assert_eq!(program(Mode::Raw, src, "STA", "ASSERT", &["I"], true), src);
    }

    #[test]
    fn detach_prelude_after_directive() {
        let p = program(Mode::Strict, "SRC", "STA", "ASSERT", &["DETACH"], true);
        assert_eq!(
            p,
            format!("\"use strict\";\n{DETACH_PRELUDE}STA\nASSERT\nDETACH\nSRC")
        );
        assert!(DETACH_PRELUDE.starts_with("var $262 = "));
        assert!(DETACH_PRELUDE.contains("ArrayBuffer.prototype.transfer"));
        assert!(DETACH_PRELUDE.contains("\"detached\").get"));
        assert!(DETACH_PRELUDE.ends_with(";\n"));
        let p = program(Mode::Async, "SRC", "STA", "ASSERT", &["DONE"], true);
        assert_eq!(
            p,
            format!("\"use strict\";\n{PRINT_PRELUDE}{DETACH_PRELUDE}STA\nASSERT\nDONE\nSRC")
        );
    }
}
