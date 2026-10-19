//! Compiler frontend: parse JavaScript or TypeScript source text into an swc
//! AST.
//!
//! This module wraps the [`swc_ecma_parser`] so that a `&str` of source can be
//! turned into a [`swc_ecma_ast::Program`]. TypeScript input has its types
//! stripped here, so later compiler passes only ever see plain ECMAScript.

use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::Program;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_transforms_typescript::strip;

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
