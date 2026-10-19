use super::parse;
use crate::error::CompileError;
use swc_ecma_ast::{Program, Stmt};

#[test]
fn parses_numeric_expression() {
    let program = parse("40 + 2").expect("valid source should parse");
    let Program::Script(script) = program else {
        unreachable!("expected a script program");
    };
    assert_eq!(script.body.len(), 1);
    assert!(
        matches!(script.body[0], Stmt::Expr(_)),
        "expected a single expression statement",
    );
}

#[test]
fn rejects_invalid_source() {
    assert!(parse("(").is_err(), "unbalanced paren should fail");
}

/// Assert `src` parses without a fatal error but is still rejected because
/// swc collected a recoverable error, surfaced as a spanned diagnostic.
fn assert_recoverable_rejected(src: &str) {
    let err = parse(src).expect_err("recoverable parse error must reject the source");
    let compile_err = err
        .downcast_ref::<CompileError>()
        .expect("parse errors are CompileErrors");
    assert!(
        compile_err.span.is_some(),
        "diagnostic must carry a span: {compile_err:?}"
    );
}

#[test]
fn rejects_ts_annotation_in_let() {
    assert_recoverable_rejected("let x: number = 1; x");
}

#[test]
fn rejects_with_in_strict_mode() {
    assert_recoverable_rejected("'use strict'; with (a) {}");
}

#[test]
fn rejects_legacy_octal_in_strict_mode() {
    assert_recoverable_rejected("'use strict'; 010");
}

#[test]
fn rejects_escaped_reserved_word() {
    assert_recoverable_rejected("n\\u{75}ll;");
}
