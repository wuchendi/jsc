//! Compile-error type carrying an optional source span.
//!
//! Every swc AST node carries a [`swc_common::Span`] (a byte range into the
//! source text). Codegen historically discarded that location and reported
//! failures as bare `anyhow!("msg")` strings. [`CompileError`] is the plumbing
//! that lets an error hold onto the offending node's span so a later stage
//! (the CLI's diagnostic renderer) can point at the exact source location.
//!
//! This module is intentionally location-*carrying* only: [`Display`] renders
//! just the message. Turning a span into a `file:line:col` caret is a separate
//! concern wired up by the CLI, which already has the original source string in
//! scope at the point it prints the error. Because `CompileError` implements
//! [`std::error::Error`] + `Send + Sync + 'static`, anyhow's blanket
//! `From<E: Error>` conversion applies: existing `?`/`anyhow!` sites keep
//! compiling, and the renderer can recover the span with
//! `err.downcast_ref::<CompileError>()`.

use std::fmt;

use swc_common::Span;

/// A compiler error that optionally remembers where in the source it occurred.
///
/// `span` is the byte range of the offending AST node when known (constructed
/// via [`CompileError::spanned`]), or `None` for errors with no meaningful
/// source location (via [`CompileError::msg`]).
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Byte range of the offending source, when a node span was available.
    pub span: Option<Span>,
    /// Human-readable error message.
    pub msg: String,
}

impl CompileError {
    /// Build an error attached to the span of an offending AST node.
    pub fn spanned(span: Span, msg: impl Into<String>) -> Self {
        Self {
            span: Some(span),
            msg: msg.into(),
        }
    }

    /// Build a span-less error (no meaningful source location).
    pub fn msg(msg: impl Into<String>) -> Self {
        Self {
            span: None,
            msg: msg.into(),
        }
    }
}

impl CompileError {
    /// Render this error as a rustc/swc-style diagnostic block pointing at the
    /// offending source, given the original `src` string and a `label` (the file
    /// path, or `<eval>` for `-e` input). Returns `None` when the error carries no
    /// span (nothing to point at) — the caller falls back to the bare message.
    ///
    /// The span's [`BytePos`](swc_common::BytePos) is translated into a byte
    /// offset into `src`: swc's first single-file source starts at `BytePos(1)`,
    /// so the source start is subtracted before indexing.
    pub fn render(&self, src: &str, label: &str) -> Option<String> {
        let span = self.span?;
        // swc's first single-file source begins at BytePos(1); shift to a 0-based
        // byte offset into `src`. Guard against underflow defensively.
        let lo = (span.lo.0.saturating_sub(1)) as usize;
        let hi = (span.hi.0.saturating_sub(1)) as usize;
        Some(render(src, label, &self.msg, lo, hi))
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Message only for now; span-aware rendering lives at the CLI boundary.
        f.write_str(&self.msg)
    }
}

impl std::error::Error for CompileError {}

/// Locate a byte `offset` within `src`, returning the 1-based line number, the
/// 1-based column (counted in characters, not bytes), and the byte range
/// `line_start..line_end` of the containing source line (excluding the trailing
/// `\n` or `\r\n`). `offset` is clamped to `src.len()` and snapped back to the
/// start of the character it falls inside, so any byte offset is accepted.
fn locate(src: &str, offset: usize) -> (usize, usize, usize, usize) {
    let offset = src.floor_char_boundary(offset);
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |n| line_start + n);
    let line_end = if src[..line_end].ends_with('\r') {
        line_end - 1
    } else {
        line_end
    };
    let col = src[line_start..offset].chars().count() + 1;
    (line, col, line_start, line_end)
}

/// Render a rustc/swc-style diagnostic block:
///
/// ```text
/// error: <msg>
///   --> <label>:LINE:COL
///    |
///  1 | <source line>
///    |              ^
/// ```
///
/// `lo`/`hi` are 0-based byte offsets into `src` (`hi` exclusive) delimiting the
/// offending span. The caret sits under the span's start column; a span wider
/// than one column underlines its full width (`^^^`, at least one caret). The
/// line-number gutter is right-aligned and the `|` bars line up. Any offsets
/// are accepted: they are clamped to `src` and snapped outward to character
/// boundaries, so a span inside a multi-byte character never panics.
pub fn render(src: &str, label: &str, msg: &str, lo: usize, hi: usize) -> String {
    let lo = src.floor_char_boundary(lo);
    let (line, col, line_start, line_end) = locate(src, lo);
    let line_text = &src[line_start..line_end];

    // Caret width: characters spanned on this line, at least one.
    let hi = src.ceil_char_boundary(hi.max(lo + 1)).min(line_end).max(lo);
    let width = src[lo..hi].chars().count().max(1);

    let num = line.to_string();
    // The bar sits one space past the number field, so the pad before an empty
    // gutter (and the `-->` line) is `digits + 1` spaces.
    let pad = " ".repeat(num.len() + 1);
    let caret_pad = " ".repeat(col - 1);
    let carets = "^".repeat(width);

    format!(
        "error: {msg}\n\
         {pad}--> {label}:{line}:{col}\n\
         {pad} |\n\
         \x20{num} | {line_text}\n\
         {pad} | {caret_pad}{carets}"
    )
}

#[cfg(test)]
mod tests {
    use super::{CompileError, locate, render};
    use swc_common::{BytePos, Span};

    #[test]
    fn locate_maps_offset_to_line_and_col_on_multiline_source() {
        let src = "let a = 1;\nlet b = 2;\nconst c = 3;";
        // Offset 0 is line 1, col 1.
        let (line, col, start, end) = locate(src, 0);
        assert_eq!((line, col), (1, 1));
        assert_eq!(&src[start..end], "let a = 1;");
        // The `b` on line 2 (byte offset of the second line's `b`).
        let b_off = src.find("b = 2").expect("has b");
        let (line, col, start, end) = locate(src, b_off);
        assert_eq!((line, col), (2, 5));
        assert_eq!(&src[start..end], "let b = 2;");
        // The `c` on line 3 (last line, no trailing newline).
        let c_off = src.rfind('c').expect("has c");
        let (line, col, start, end) = locate(src, c_off);
        assert_eq!((line, col), (3, 7));
        assert_eq!(&src[start..end], "const c = 3;");
    }

    #[test]
    fn render_points_caret_at_span_start() {
        let src = "const c = 1; c = 2";
        // The second `c` is a single-char span at byte offset 13.
        let lo = 13;
        let block = render(
            src,
            "<eval>",
            "cannot assign to constant binding: c",
            lo,
            lo + 1,
        );
        let expected = "error: cannot assign to constant binding: c\n  \
             --> <eval>:1:14\n   |\n 1 | const c = 1; c = 2\n   |              ^";
        assert_eq!(block, expected);
    }

    #[test]
    fn render_underlines_full_multichar_span() {
        let src = "let ab = 1; ab = 2";
        // The second `ab` spans two columns starting at byte offset 12.
        let lo = 12;
        let block = render(src, "<eval>", "nope", lo, lo + 2);
        assert!(block.contains(":1:13"), "col should be 13, got: {block}");
        assert!(
            block.ends_with("^^"),
            "a 2-column span underlines two carets, got: {block}"
        );
    }

    #[test]
    fn render_reports_line_below_the_first() {
        let src = "let a = 1;\nconst c = 2; c = 3";
        let lo = src.rfind('c').expect("has c");
        let block = render(
            src,
            "file.js",
            "cannot assign to constant binding: c",
            lo,
            lo + 1,
        );
        assert!(block.contains("--> file.js:2:14"), "got: {block}");
        assert!(
            block.contains("\n 2 | const c = 2; c = 3\n"),
            "got: {block}"
        );
    }

    #[test]
    fn render_method_translates_bytepos_base() {
        // A CompileError span uses swc BytePos (first source at BytePos(1)); the
        // renderer subtracts the base so the caret lands on the right column.
        let src = "const c = 1; c = 2";
        // BytePos of the second `c`: 0-based offset 13 + base 1 = 14.
        let span = Span::new(BytePos(14), BytePos(15));
        let err = CompileError::spanned(span, "cannot assign to constant binding: c");
        let block = err.render(src, "<eval>").expect("spanned error renders");
        assert!(block.contains("--> <eval>:1:14"), "got: {block}");
        assert!(block.contains('^'), "got: {block}");
        // A span-less error has nothing to point at.
        assert!(CompileError::msg("boom").render(src, "<eval>").is_none());
    }

    #[test]
    fn render_never_panics_at_any_byte_offset_of_non_ascii_source() {
        // 2-byte (é), 3-byte (U+180E, 中), 4-byte (😀) chars, CRLF and LF line
        // endings, and an empty last line. Every `lo` and every `lo..hi` pair —
        // including offsets inside a multi-byte char and past the end — must
        // render without panicking.
        let src = "var a\u{180e}= 1;\r\nlet é = '中';\n// 😀x\r\n\n";
        let len = src.len();
        for lo in 0..=len + 2 {
            let (_, col, start, end) = locate(src, lo);
            assert!(col >= 1 && start <= end && end <= len);
            for hi in 0..=len + 2 {
                let block = render(src, "f.js", "boom", lo, hi);
                assert!(block.starts_with("error: boom\n"), "lo={lo} hi={hi}");
            }
        }
    }

    #[test]
    fn render_inside_multibyte_char_points_at_that_char() {
        // Offset 6 is the middle byte of U+180E (bytes 5..8): the caret snaps
        // back to the char's start column (6) and underlines one char.
        let src = "var a\u{180e}= 1;";
        let block = render(src, "f.js", "boom", 6, 7);
        assert!(block.contains("--> f.js:1:6"), "got: {block}");
        assert!(block.ends_with("     ^"), "got: {block}");
    }

    #[test]
    fn render_strips_carriage_return_from_crlf_line() {
        let src = "let a;\r\nlet b;\r\n";
        let lo = src.find('b').expect("has b");
        let block = render(src, "f.js", "boom", lo, lo + 1);
        assert!(block.contains("\n 2 | let b;\n"), "got: {block:?}");
    }

    #[test]
    fn spanned_carries_span_and_message() {
        let span = Span::new(BytePos(3), BytePos(7));
        let err = CompileError::spanned(span, "unsupported statement");
        assert_eq!(err.span, Some(span));
        assert_eq!(err.msg, "unsupported statement");
        // Display is the bare message (rich rendering is a later stage).
        assert_eq!(err.to_string(), "unsupported statement");
    }

    #[test]
    fn msg_has_no_span() {
        let err = CompileError::msg("boom");
        assert_eq!(err.span, None);
        assert_eq!(err.msg, "boom");
    }

    #[test]
    fn converts_into_anyhow_and_downcasts_back() {
        // Proves the span survives the anyhow boundary so the CLI renderer can
        // recover it with `downcast_ref`.
        let span = Span::new(BytePos(1), BytePos(5));
        let err: anyhow::Error = CompileError::spanned(span, "nope").into();
        let recovered = err
            .downcast_ref::<CompileError>()
            .expect("should downcast back to CompileError");
        assert_eq!(recovered.span, Some(span));
        assert_eq!(recovered.msg, "nope");
    }

    #[test]
    fn unsupported_statement_error_carries_a_span() {
        // End-to-end proof that the converted codegen site attaches a real node
        // span: `debugger;` is an unsupported statement.
        let program =
            crate::frontend::parse("debugger;", crate::frontend::Dialect::Js).expect("parses");
        let err = crate::codegen::lower(&program).expect_err("should reject");
        let compile_err = err
            .downcast_ref::<CompileError>()
            .expect("codegen error should be a CompileError");
        assert!(
            compile_err.span.is_some(),
            "unsupported-statement error should carry the offending node span"
        );
        // The message is a clean human phrase, not a raw `{:?}` AST dump.
        assert_eq!(compile_err.msg, "debugger statements are not supported");
        assert!(!compile_err.msg.contains('{'), "no AST-debug leakage");
    }
}
