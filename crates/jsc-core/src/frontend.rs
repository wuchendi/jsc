//! Compiler frontend: parse JavaScript or TypeScript source text into an swc
//! AST.
//!
//! This module wraps the [`swc_ecma_parser`] so that a `&str` of source can be
//! turned into a [`swc_ecma_ast::Program`]. TypeScript input has its types
//! stripped here, so later compiler passes only ever see plain ECMAScript.

use swc_common::{BytePos, FileName, GLOBALS, Globals, Mark, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::{ClassMethod, MethodProp, Program};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_transforms_typescript::strip;
use swc_ecma_visit::{Visit, VisitWith};

use crate::error::CompileError;

/// Source language accepted by [`parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// ECMAScript.
    Js,
    /// TypeScript (no TSX); types are stripped before returning.
    Ts,
}

/// Parse `src` in `dialect` into an swc [`Program`].
///
/// The returned [`swc_ecma_ast::Program`] is swc's top-level AST node — either
/// a `Module` or a `Script` depending on the source — and is always plain
/// ECMAScript: [`Dialect::Ts`] input is run through swc's resolver and
/// TypeScript strip pass. Both fatal and recoverable swc parse errors reject
/// the source as a spanned [`CompileError`] rather than panicking or silently
/// accepting invalid input.
pub fn parse(src: &str, dialect: Dialect) -> anyhow::Result<Program> {
    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(Lrc::new(FileName::Anon), src.to_string());

    let syntax = match dialect {
        Dialect::Js => Syntax::Es(EsSyntax::default()),
        Dialect::Ts => Syntax::Typescript(TsSyntax::default()),
    };
    let lexer = Lexer::new(
        syntax,
        swc_ecma_ast::EsVersion::default(),
        StringInput::from(&*fm),
        None,
    );

    let mut parser = Parser::new_from(lexer);
    // Route swc parser failures through the SAME spanned-diagnostic path as
    // codegen rejections: the parser error carries a source span (via
    // `Spanned`) and a human-readable `kind().msg()`, so the CLI can render a
    // caret block pointing at the offending location. A fatal error wins (it
    // is the most relevant); otherwise swc may still have collected
    // recoverable errors (e.g. strict-mode early errors), and the first one in
    // source order rejects the program.
    let to_compile_error = |e: swc_ecma_parser::error::Error| {
        CompileError::spanned(e.span(), e.kind().msg().into_owned())
    };
    let mut program = parser.parse_program().map_err(to_compile_error)?;
    if let Some(e) = parser.take_errors().into_iter().next() {
        return Err(to_compile_error(e).into());
    }
    check_async_methods(&program, src, fm.start_pos)?;
    if dialect == Dialect::Ts {
        GLOBALS.set(&Globals::new(), || {
            let unresolved_mark = Mark::new();
            let top_level_mark = Mark::new();
            program.mutate(resolver(unresolved_mark, top_level_mark, true));
            program.mutate(strip(unresolved_mark, top_level_mark));
        });
    }
    Ok(program)
}

#[cfg(test)]
mod tests;

/// Reject an `async` method whose `async` is followed by a line terminator
/// before its name (`async [no LineTerminator here] PropertyName`), which
/// swc accepts: the SyntaxError points at the method name.
fn check_async_methods(program: &Program, src: &str, start: BytePos) -> anyhow::Result<()> {
    /// Collects the name span of each async method.
    struct Scan(Vec<Span>);
    impl Visit for Scan {
        fn visit_method_prop(&mut self, node: &MethodProp) {
            if node.function.is_async {
                self.0.push(node.key.span());
            }
            node.visit_children_with(self);
        }
        fn visit_class_method(&mut self, node: &ClassMethod) {
            if node.function.is_async {
                self.0.push(node.key.span());
            }
            node.visit_children_with(self);
        }
    }
    let mut scan = Scan(Vec::new());
    program.visit_with(&mut scan);
    for key in scan.0 {
        let end = usize::try_from((key.lo - start).0).unwrap_or(usize::MAX);
        if line_break_after_async(src.get(..end).unwrap_or_default()) {
            return Err(CompileError::spanned(
                key,
                "no line terminator is allowed between `async` and the method name",
            )
            .into());
        }
    }
    Ok(())
}

/// Whether the source `head` before an async method's name ends with `async`
/// followed by whitespace, comments and an optional `*` that contain a line
/// terminator.
fn line_break_after_async(head: &str) -> bool {
    let is_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let mut rest = head;
    let mut broke = false;
    loop {
        let trimmed = rest.trim_end_matches(|c: char| c.is_whitespace() || c == '*');
        broke |= rest[trimmed.len()..].contains(is_break);
        rest = trimmed;
        match rest
            .strip_suffix("*/")
            .and_then(|r| r.rfind("/*").map(|at| &r[..at]))
        {
            Some(before) => {
                broke |= rest[before.len()..].contains(is_break);
                rest = before;
            }
            None => break,
        }
    }
    broke && rest.ends_with("async")
}
