use super::parse;
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
