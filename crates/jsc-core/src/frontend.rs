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
/// a `Module` or a `Script` depending on the source. Parse failures are mapped
/// into [`anyhow::Error`] rather than panicking.
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
    // caret block pointing at the offending location. If the parser produced
    // several errors, this surfaces the first (most-relevant) fatal one.
    parser
        .parse_program()
        .map_err(|e| CompileError::spanned(e.span(), e.kind().msg().into_owned()).into())
}

#[cfg(test)]
mod tests;
