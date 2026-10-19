use super::{Dialect, parse};
use crate::error::CompileError;
use swc_ecma_ast::{Program, Stmt};

#[test]
fn parses_numeric_expression() {
    let program = parse("40 + 2", Dialect::Js).expect("valid source should parse");
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
    assert!(
        parse("(", Dialect::Js).is_err(),
        "unbalanced paren should fail"
    );
}

/// Assert `src` parses without a fatal error but is still rejected because
/// swc collected a recoverable error, surfaced as a spanned diagnostic.
fn assert_recoverable_rejected(src: &str) {
    let err = parse(src, Dialect::Js).expect_err("recoverable parse error must reject the source");
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

/// Parse `src` as TypeScript and lower the stripped program, proving the
/// result is plain ES that codegen accepts.
fn lower_ts(src: &str) -> anyhow::Result<crate::ir::IrProgram> {
    crate::codegen::lower(&parse(src, Dialect::Ts)?)
}

#[test]
fn ts_dialect_strips_type_annotations() {
    let program = parse(
        "interface P { x: number }\ntype N = number;\nlet n: N = 1 as number;\nn!",
        Dialect::Ts,
    )
    .expect("typed source should parse as TS");
    let Program::Script(script) = program else {
        unreachable!("expected a script program");
    };
    // `interface` and `type` vanish; the `let` and the expression remain.
    assert_eq!(
        script.body.len(),
        2,
        "type-only declarations must be stripped"
    );
    assert!(matches!(script.body[1], Stmt::Expr(_)));
}

#[test]
fn ts_dialect_output_lowers() {
    lower_ts("function id<T>(x: T): T { return x }\nconst y: number = id<number>(41)!;\ny + 1")
        .expect("stripped TS must lower");
}

#[test]
fn ts_dialect_rejects_invalid_source() {
    let err = parse("let x: = 1", Dialect::Ts).expect_err("invalid TS must fail");
    assert!(err.downcast_ref::<CompileError>().is_some());
}
