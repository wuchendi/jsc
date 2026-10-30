use super::{BinOp, IrNode, PropKey};

#[test]
fn builds_binary_expression() {
    let node = IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Num(40.0)),
        rhs: Box::new(IrNode::Num(2.0)),
    };

    let IrNode::Binary { op, lhs, rhs } = &node else {
        unreachable!("expected a binary node");
    };
    assert_eq!(*op, BinOp::Add);
    assert_eq!(**lhs, IrNode::Num(40.0));
    assert_eq!(**rhs, IrNode::Num(2.0));
}

#[test]
fn partial_eq_distinguishes_operators() {
    let add = IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Num(1.0)),
        rhs: Box::new(IrNode::Num(2.0)),
    };
    let sub = IrNode::Binary {
        op: BinOp::Sub,
        lhs: Box::new(IrNode::Num(1.0)),
        rhs: Box::new(IrNode::Num(2.0)),
    };

    assert_eq!(add, add.clone());
    assert_ne!(add, sub);
}

#[test]
fn debug_output_is_non_empty() {
    let node = IrNode::Num(3.5);
    assert_ne!(format!("{node:?}"), "");
}

#[test]
fn builds_object_literal() {
    let node = IrNode::Object {
        entries: vec![(PropKey::Static("a".into()), IrNode::Num(1.0))],
        proto: None,
        class: crate::abi::CLASS_ORDINARY,
        reserve: 0,
    };
    let IrNode::Object { entries, .. } = &node else {
        unreachable!("expected an object node");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, PropKey::Static("a".into()));
    assert_eq!(entries[0].1, IrNode::Num(1.0));
}
