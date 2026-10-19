//! Compiler frontend: parse JavaScript source text into an swc AST.
//!
//! This module wraps the [`swc_ecma_parser`] so that a `&str` of source can be
//! turned into a [`swc_ecma_ast::Program`]. The MVP targets numeric
//! expressions, but the underlying parser handles general ECMAScript; this
//! stage simply returns the parsed program for later compiler passes.

use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::Program;
use swc_ecma_parser::{Parser, StringInput, Syntax, lexer::Lexer};

use crate::error::CompileError;

/// Parse JavaScript `src` into an swc [`Program`].
///
/// The returned [`swc_ecma_ast::Program`] is swc's top-level AST node — either
/// a `Module` or a `Script` depending on the source. Both fatal and recoverable
/// swc parse errors reject the source as a spanned [`CompileError`] rather
/// than panicking or silently accepting invalid input.
pub fn parse(src: &str) -> anyhow::Result<Program> {
    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(Lrc::new(FileName::Anon), src.to_string());

    let lexer = Lexer::new(
        Syntax::Es(swc_ecma_parser::EsSyntax::default()),
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
    let program = parser.parse_program().map_err(to_compile_error)?;
    match parser.take_errors().into_iter().next() {
        Some(e) => Err(to_compile_error(e).into()),
        None => Ok(program),
    }
}

#[cfg(test)]
mod tests;
