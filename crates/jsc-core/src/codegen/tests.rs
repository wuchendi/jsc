use super::lower;
use crate::frontend::parse;
use crate::ir::{BinOp, IrNode, IrStmt, OpSetKind};

fn lower_src(src: &str) -> anyhow::Result<crate::ir::IrProgram> {
    lower(&parse(src)?)
}

/// Lower a single-expression-statement program and return its expression.
fn lower_expr(src: &str) -> anyhow::Result<IrNode> {
    let program = lower_src(src)?;
    let [IrStmt::Expr(node)] = program.stmts.as_slice() else {
        return Err(anyhow::anyhow!("expected a single expression statement"));
    };
    Ok(node.clone())
}

#[test]
fn lowers_addition() {
    let node = lower_expr("40 + 2").expect("should lower");
    assert_eq!(
        node,
        IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Num(40.0)),
            rhs: Box::new(IrNode::Num(2.0)),
        }
    );
}

#[test]
fn lowers_boolean_literals() {
    assert_eq!(
        lower_expr("true").expect("should lower"),
        IrNode::Bool(true)
    );
    assert_eq!(
        lower_expr("false").expect("should lower"),
        IrNode::Bool(false)
    );
}

#[test]
fn lowers_string_literal() {
    // A string literal lowers to `IrNode::Str` carrying the decoded text.
    assert_eq!(
        lower_expr("\"foo\"").expect("should lower"),
        IrNode::Str("foo".to_owned())
    );
}

#[test]
fn lowers_null_literal() {
    assert_eq!(lower_expr("null").expect("should lower"), IrNode::Null);
}

#[test]
fn lowers_undefined_identifier() {
    assert_eq!(
        lower_expr("undefined").expect("should lower"),
        IrNode::Undefined
    );
}

#[test]
fn declared_local_shadows_undefined() {
    // A local named `undefined` resolves to the local, not the literal.
    let program = lower_src("let undefined = 5; undefined").expect("should lower");
    assert_eq!(program.stmts[1], IrStmt::Expr(IrNode::Local(0)));
}

#[test]
fn respects_precedence() {
    let node = lower_expr("1 + 2 * 3").expect("should lower");
    assert_eq!(
        node,
        IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Binary {
                op: BinOp::Mul,
                lhs: Box::new(IrNode::Num(2.0)),
                rhs: Box::new(IrNode::Num(3.0)),
            }),
        }
    );
}

#[test]
fn respects_parentheses() {
    let node = lower_expr("(1 + 2) * 3").expect("should lower");
    assert_eq!(
        node,
        IrNode::Binary {
            op: BinOp::Mul,
            lhs: Box::new(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Num(1.0)),
                rhs: Box::new(IrNode::Num(2.0)),
            }),
            rhs: Box::new(IrNode::Num(3.0)),
        }
    );
}

#[test]
fn folds_unary_minus_literal() {
    let node = lower_expr("-5").expect("should lower");
    assert_eq!(node, IrNode::Num(-5.0));
}

#[test]
fn unary_plus_is_transparent() {
    let node = lower_expr("+7").expect("should lower");
    assert_eq!(node, IrNode::Num(7.0));
}

#[test]
fn lowers_ternary_operator() {
    let node = lower_expr("true ? 1 : 2").expect("should lower");
    assert_eq!(
        node,
        IrNode::Ternary {
            cond: Box::new(IrNode::Bool(true)),
            then_branch: Box::new(IrNode::Num(1.0)),
            else_branch: Box::new(IrNode::Num(2.0)),
        }
    );
}

#[test]
fn lowers_object_literal() {
    // Identifier and string-literal keys both lower to `(key, value)` pairs.
    let program = lower_src("let o = {a: 1, \"b\": 2}; o").expect("should lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected a declaration");
    };
    assert_eq!(
        *init,
        IrNode::Object(vec![
            ("a".to_owned(), IrNode::Num(1.0)),
            ("b".to_owned(), IrNode::Num(2.0)),
        ])
    );
}

#[test]
fn lowers_error_constructor_to_object() {
    // `new TypeError("bad")` lowers to a branded error object: `name`, `message`,
    // the derived `stack`, and the hidden `@@errorkind` marker. The message is
    // teed into a fresh temp (evaluated once) and reused by `stack`.
    let program = lower_src("new TypeError(\"bad\")").expect("should lower");
    let IrStmt::Expr(IrNode::Object(entries)) = &program.stmts[0] else {
        unreachable!("expected an object literal");
    };
    let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["name", "message", "stack", crate::abi::ERROR_KIND_KEY]
    );
    assert_eq!(entries[0].1, IrNode::Str("TypeError".to_owned()));
    assert_eq!(entries[3].1, IrNode::Num(crate::abi::ERROR_KIND_TYPE));
    let IrNode::Assign { local, value } = &entries[1].1 else {
        unreachable!("message should tee into a temp");
    };
    assert_eq!(**value, IrNode::Str("bad".to_owned()));
    assert_eq!(
        entries[2].1,
        IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str("TypeError: ".to_owned())),
            rhs: Box::new(IrNode::Local(*local)),
        }
    );
}

#[test]
fn lowers_error_no_arg_to_empty_message() {
    // `new Error()` defaults `message` to "" and `stack` to just the name (no
    // trailing colon), still carrying the hidden `@@errorkind` marker.
    let program = lower_src("new Error()").expect("should lower");
    assert_eq!(
        program.stmts[0],
        IrStmt::Expr(IrNode::Object(vec![
            ("name".to_owned(), IrNode::Str("Error".to_owned())),
            ("message".to_owned(), IrNode::Str(String::new())),
            ("stack".to_owned(), IrNode::Str("Error".to_owned())),
            (
                crate::abi::ERROR_KIND_KEY.to_owned(),
                IrNode::Num(crate::abi::ERROR_KIND_ERROR)
            ),
        ]))
    );
}

#[test]
fn rejects_unknown_constructor() {
    // Only the three built-in error constructors are in scope.
    assert!(lower_src("new Widget(1)").is_err());
}

#[test]
fn lowers_static_property_read_as_index() {
    // `o.a` lowers to `o["a"]` — an `Index` node with a string key.
    let program = lower_src("let o = {a: 1}; o.a").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::Index {
            obj: Box::new(IrNode::Local(0)),
            index: Box::new(IrNode::Str("a".to_owned())),
        })
    );
}

#[test]
fn lowers_static_property_assignment_as_indexset() {
    // `o.a = v` lowers to `o["a"] = v` — an `IndexSet` node with a string key.
    let program = lower_src("let o = {a: 1}; o.a = 7").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::IndexSet {
            obj: Box::new(IrNode::Local(0)),
            index: Box::new(IrNode::Str("a".to_owned())),
            value: Box::new(IrNode::Num(7.0)),
        })
    );
}

#[test]
fn rejects_length_assignment() {
    // `.length` is a computed header read; assigning to it stays an error.
    assert!(
        lower_src("let a = [1, 2]; a.length = 5").is_err(),
        "assigning to .length is unsupported"
    );
}

#[test]
fn lowers_shorthand_object_property() {
    // Shorthand `{a}` ≡ `{a: a}` is supported.
    assert!(
        lower_src("let a = 1; let o = {a}; o").is_ok(),
        "shorthand object property should lower"
    );
}

#[test]
fn rejects_computed_object_key() {
    // Computed keys `{[e]: v}` are out of scope.
    assert!(
        lower_src("let k = \"a\"; let o = {[k]: 1}; o").is_err(),
        "computed object key is unsupported"
    );
}

#[test]
fn lowers_typeof_operator() {
    let node = lower_expr("typeof true").expect("should lower");
    assert_eq!(node, IrNode::TypeOf(Box::new(IrNode::Bool(true))));
}

#[test]
fn lowers_logical_not() {
    let node = lower_expr("!true").expect("should lower");
    assert_eq!(node, IrNode::Not(Box::new(IrNode::Bool(true))));
}

#[test]
fn lowers_logical_operators() {
    use crate::ir::LogicalOp;
    assert_eq!(
        lower_expr("1 && 2").expect("should lower"),
        IrNode::Logical {
            op: LogicalOp::And,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Num(2.0)),
        }
    );
    assert_eq!(
        lower_expr("1 || 2").expect("should lower"),
        IrNode::Logical {
            op: LogicalOp::Or,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Num(2.0)),
        }
    );
    assert_eq!(
        lower_expr("1 ?? 2").expect("should lower"),
        IrNode::Logical {
            op: LogicalOp::Nullish,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Num(2.0)),
        }
    );
}

#[test]
fn lowers_declaration_and_reference() {
    let program = lower_src("let x = 40; x + 2").expect("should lower");
    assert_eq!(program.locals, 1);
    assert_eq!(
        program.stmts,
        vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Num(40.0),
            },
            IrStmt::Expr(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(0)),
                rhs: Box::new(IrNode::Num(2.0)),
            }),
        ]
    );
}

#[test]
fn lowers_assignment_to_tee() {
    let program = lower_src("let x = 1; x = 5").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::Assign {
            local: 0,
            value: Box::new(IrNode::Num(5.0)),
        })
    );
}

#[test]
fn lowers_compound_assignment_to_binary_over_local() {
    // `x += 2` desugars to `x = x + 2`, reading the target once as a local.
    let program = lower_src("let x = 40; x += 2").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::Assign {
            local: 0,
            value: Box::new(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(0)),
                rhs: Box::new(IrNode::Num(2.0)),
            }),
        })
    );
}

#[test]
fn lowers_mod_compound_assignment() {
    // `w %= 5` desugars to `w = w % 5`.
    let program = lower_src("let w = 17; w %= 5").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::Assign {
            local: 0,
            value: Box::new(IrNode::Binary {
                op: BinOp::Mod,
                lhs: Box::new(IrNode::Local(0)),
                rhs: Box::new(IrNode::Num(5.0)),
            }),
        })
    );
}

#[test]
fn lowers_prefix_and_postfix_update() {
    // `x++` yields the old value; `++x` yields the new value.
    let postfix = lower_src("let x = 5; x++").expect("should lower");
    assert_eq!(
        postfix.stmts[1],
        IrStmt::Expr(IrNode::IncDec {
            local: 0,
            inc: true,
            prefix: false,
        })
    );
    let prefix = lower_src("let x = 5; ++x").expect("should lower");
    assert_eq!(
        prefix.stmts[1],
        IrStmt::Expr(IrNode::IncDec {
            local: 0,
            inc: true,
            prefix: true,
        })
    );
    // `--i` is a prefix decrement.
    let dec = lower_src("let i = 5; --i").expect("should lower");
    assert_eq!(
        dec.stmts[1],
        IrStmt::Expr(IrNode::IncDec {
            local: 0,
            inc: false,
            prefix: true,
        })
    );
}

#[test]
fn lowers_standalone_mod_operator() {
    let node = lower_expr("17 % 5").expect("should lower");
    assert_eq!(
        node,
        IrNode::Binary {
            op: BinOp::Mod,
            lhs: Box::new(IrNode::Num(17.0)),
            rhs: Box::new(IrNode::Num(5.0)),
        }
    );
}

#[test]
fn lowers_element_compound_assignment_as_indexopset() {
    // `a[0] += 1` lowers to an `IndexOpSet` evaluating base+index once into
    // fresh temps.
    let program = lower_src("let a = [1, 2]; a[0] += 1").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::IndexOpSet {
            obj: Box::new(IrNode::Local(0)),
            index: Box::new(IrNode::Num(0.0)),
            op: BinOp::Add,
            rhs: Box::new(IrNode::Num(1.0)),
            obj_local: 1,
            idx_local: 2,
            new_local: 3,
            kind: OpSetKind::Compound,
        })
    );
}

#[test]
fn lowers_static_property_compound_assignment_as_indexopset() {
    // `o.x -= 1` lowers to `o["x"] -= 1` — an `IndexOpSet` with a string key.
    let program = lower_src("let o = {x: 1}; o.x -= 1").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::IndexOpSet {
            obj: Box::new(IrNode::Local(0)),
            index: Box::new(IrNode::Str("x".to_owned())),
            op: BinOp::Sub,
            rhs: Box::new(IrNode::Num(1.0)),
            obj_local: 1,
            idx_local: 2,
            new_local: 3,
            kind: OpSetKind::Compound,
        })
    );
}

#[test]
fn rejects_length_compound_assignment() {
    // `.length op= e` stays an error (as does plain `.length =`).
    assert!(
        lower_src("let a = [1, 2]; a.length += 1").is_err(),
        "compound assigning to .length is unsupported"
    );
}

#[test]
fn lowers_member_update_as_indexopset() {
    // `a[0]++` lowers to a postfix-update `IndexOpSet` adding 1.
    let program = lower_src("let a = [1]; a[0]++").expect("should lower");
    assert_eq!(
        program.stmts[1],
        IrStmt::Expr(IrNode::IndexOpSet {
            obj: Box::new(IrNode::Local(0)),
            index: Box::new(IrNode::Num(0.0)),
            op: BinOp::Add,
            rhs: Box::new(IrNode::Num(1.0)),
            obj_local: 1,
            idx_local: 2,
            new_local: 3,
            kind: OpSetKind::PostfixUpdate,
        })
    );
}

#[test]
fn rejects_undeclared_identifier() {
    assert!(
        lower_src("a + 1").is_err(),
        "undeclared identifier is an error"
    );
}

#[test]
fn rejects_assignment_to_undeclared() {
    assert!(
        lower_src("x = 1").is_err(),
        "assigning an undeclared var errors"
    );
}

#[test]
fn declaration_without_initializer_binds_undefined() {
    // P0.4: an uninitialized `let x;` binds `undefined` (JS semantics) rather
    // than being rejected, so `x ??= v` can start from an undefined target.
    let program = lower_src("let x;").expect("should lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected a declaration");
    };
    assert_eq!(*init, IrNode::Undefined);
}

#[test]
fn rejects_unsupported_operator() {
    // `%` and `**` (exponentiation) are supported; bitwise
    // `&` (and the other bitwise/shift operators) remains unsupported.
    assert!(lower_src("2 & 3").is_err(), "bitwise AND is unsupported");
}

#[test]
fn rejects_async_and_unsupported_generator_functions() {
    // An `async` function EXPRESSION / ARROW would otherwise lower as an
    // ordinary function and run to a wrong result, so each must be a clear
    // compile error. A TOP-LEVEL `async function` declaration and a TOP-LEVEL
    // `function*` declaration are supported; a generator EXPRESSION / ARROW and
    // an `async` generator remain out of scope.
    let msg = |src: &str| format!("{:#}", lower_src(src).unwrap_err());
    assert!(msg("let f = async function(){ return 1 };").contains("async"));
    assert!(msg("let f = async () => 1;").contains("async"));
    assert!(msg("let g = function*(){ return 1 };").contains("generator"));
    // A top-level `function* g(){}` declaration compiles, as does a top-level
    // `async function f(){}` declaration.
    assert!(lower_src("function* g(){ yield 1 } g().next();").is_ok());
    assert!(lower_src("async function f(){ return 1 } f();").is_ok());
}

#[test]
fn lowers_comparison_operator() {
    let node = lower_expr("1 < 2").expect("should lower");
    assert_eq!(
        node,
        IrNode::Binary {
            op: BinOp::Lt,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Num(2.0)),
        }
    );
}

#[test]
fn lowers_if_else() {
    let program = lower_src("let x = 0; if (1 < 2) { x = 1 } else { x = 2 }").expect("lower");
    let IrStmt::If {
        then_body,
        else_body,
        ..
    } = &program.stmts[1]
    else {
        unreachable!("expected an if statement");
    };
    assert_eq!(then_body.len(), 1);
    assert_eq!(else_body.len(), 1);
}

#[test]
fn lowers_function_declaration_and_call() {
    let program = lower_src("function add(a, b) { return a + b } add(1, 2)").expect("lower");
    // The function declaration is hoisted out of `main`; only the call
    // remains at the top level.
    assert_eq!(program.functions.len(), 1);
    let func = &program.functions[0];
    // Uniform ABI (env-last): `max_arity` (= 2) argument slots (locals 0, 1),
    // then the env slot (local 2) and the argument-count slot (local 3),
    // so `a`/`b` are locals 0/1 and `params` is 4.
    assert_eq!(func.params, 4);
    assert_eq!(func.locals, 4);
    assert_eq!(
        func.body,
        vec![IrStmt::Return(Some(IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Local(0)),
            rhs: Box::new(IrNode::Local(1)),
        }))]
    );
    // A call to a function *name* keeps the direct-call form, resolving to
    // wasm function index 1 (index 0 is `main`).
    assert_eq!(
        program.stmts,
        vec![IrStmt::Expr(IrNode::Call {
            func: 1,
            args: vec![IrNode::Num(1.0), IrNode::Num(2.0)],
        })]
    );
}

#[test]
fn lowers_function_name_in_value_position_to_func_value() {
    // `let f = add;` binds `f` to a first-class function value (table index
    // 0 = wasm function index 1 minus one).
    let program = lower_src("function add(a, b) { return a + b } let f = add; f").expect("lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected the `let f = add` declaration");
    };
    assert_eq!(
        *init,
        IrNode::FuncValue {
            table_index: 0,
            captures: vec![]
        }
    );
}

#[test]
fn lowers_call_through_variable_to_call_indirect() {
    // Calling a variable that holds a function value is an indirect call.
    let program =
        lower_src("function add(a, b) { return a + b } let f = add; f(40, 2)").expect("lower");
    let IrStmt::Expr(node) = program.stmts.last().expect("a trailing expression") else {
        unreachable!("expected a trailing expression statement");
    };
    assert_eq!(
        *node,
        IrNode::CallIndirect {
            callee: Box::new(IrNode::Local(0)),
            args: vec![IrNode::Num(40.0), IrNode::Num(2.0)],
        }
    );
}

#[test]
fn passing_a_function_argument_lowers_to_func_value() {
    // `apply(dbl, 21)` passes `dbl` as a function value; `apply` is called
    // directly by name, and its `f(x)` body call is indirect.
    let program = lower_src(
        "function apply(f, x) { return f(x) } function dbl(n) { return n * 2 } apply(dbl, 21)",
    )
    .expect("lower");
    let IrStmt::Expr(IrNode::Call { func, args }) = program.stmts.last().expect("a trailing call")
    else {
        unreachable!("expected a direct call to `apply`");
    };
    assert_eq!(*func, 1);
    // `dbl` (wasm index 2) becomes function-table index 1 in value position.
    assert_eq!(
        args[0],
        IrNode::FuncValue {
            table_index: 1,
            captures: vec![]
        }
    );
    assert_eq!(args[1], IrNode::Num(21.0));
    // `apply`'s body calls its `f` parameter indirectly (`f`/`x` are the
    // env-last ABI's argument slots, locals 0/1).
    let apply = &program.functions[0];
    assert_eq!(
        apply.body,
        vec![IrStmt::Return(Some(IrNode::CallIndirect {
            callee: Box::new(IrNode::Local(0)),
            args: vec![IrNode::Local(1)],
        }))]
    );
}

#[test]
fn arrow_lifts_to_func_value_and_registers_a_function() {
    // An arrow expression lowers to a `FuncValue` (the first table slot,
    // index 0, since there are no declarations) and appends its body to the
    // functions vector.
    let program = lower_src("let sq = x => x*x; sq(7)").expect("lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected the `let sq = ..` declaration");
    };
    assert_eq!(
        *init,
        IrNode::FuncValue {
            table_index: 0,
            captures: vec![]
        }
    );
    assert_eq!(program.functions.len(), 1);
}

#[test]
fn arrow_expression_body_desugars_to_return() {
    // An expression-bodied arrow's body is a single implicit `return` reading
    // its first argument slot (local 0 under the env-last ABI).
    let program = lower_src("let sq = x => x*x").expect("lower");
    let arrow = &program.functions[0];
    assert_eq!(
        arrow.body,
        vec![IrStmt::Return(Some(IrNode::Binary {
            op: BinOp::Mul,
            lhs: Box::new(IrNode::Local(0)),
            rhs: Box::new(IrNode::Local(0)),
        }))]
    );
}

#[test]
fn arrow_block_body_lowers_statements_directly() {
    // A block-bodied arrow lowers its statements as an ordinary function body
    // (a local declaration followed by an explicit `return`), with no implicit
    // return wrapper.
    let program = lower_src("let f = (n) => { let t = n*2; return t+1 }").expect("lower");
    let arrow = &program.functions[0];
    assert!(matches!(
        arrow.body.as_slice(),
        [IrStmt::Decl { .. }, IrStmt::Return(Some(_))]
    ));
}

#[test]
fn arrow_single_and_parenthesized_params_bind_identically() {
    // `x => x` and `(x) => x` lower to the same body reading the first
    // argument slot (local 0 under the env-last ABI).
    let unparen = lower_src("let id = x => x").expect("lower");
    let paren = lower_src("let id = (x) => x").expect("lower");
    let expected = vec![IrStmt::Return(Some(IrNode::Local(0)))];
    assert_eq!(unparen.functions[0].body, expected);
    assert_eq!(paren.functions[0].body, expected);
}

#[test]
fn arrow_arity_widens_the_uniform_abi() {
    // A three-parameter arrow widens `K` to 3, so every lifted function has
    // `params = 3 args + env + argc = 5`, shared across mixed-arity arrows.
    let program = lower_src("let f=(a,b,c)=>a+b+c; let g=x=>x").expect("lower");
    assert_eq!(program.functions.len(), 2);
    assert!(program.functions.iter().all(|f| f.params == 5));
}

#[test]
fn nested_arrow_is_lifted_after_its_enclosing_arrow() {
    // A nested arrow is registered as its own function; the outer arrow takes
    // the first table slot (reserved before its body is lowered), the inner
    // the second.
    let program = lower_src("let outer = a => (b => b)").expect("lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected the `let outer = ..` declaration");
    };
    assert_eq!(
        *init,
        IrNode::FuncValue {
            table_index: 0,
            captures: vec![]
        }
    );
    assert_eq!(program.functions.len(), 2);
}

#[test]
fn capturing_param_is_boxed_and_env_is_built() {
    // `function adder(x){ return y => x + y }`: `x` is captured by the arrow,
    // so `adder` boxes its `x` parameter into a cell (a prologue statement)
    // and the returned `FuncValue` carries the cell as its one capture.
    let program = lower_src("function adder(x){ return y => x + y }").expect("lower");
    let adder = &program.functions[0];
    // Prologue: move the incoming `x` (local 0) into a fresh cell.
    assert_eq!(
        adder.body[0],
        IrStmt::Expr(IrNode::Assign {
            local: 0,
            value: Box::new(IrNode::CellNew(Box::new(IrNode::Local(0)))),
        })
    );
    // The returned function value captures the `x` cell (sourced from the
    // enclosing local 0).
    let IrStmt::Return(Some(IrNode::FuncValue { captures, .. })) = &adder.body[1] else {
        unreachable!("expected a returned function value");
    };
    assert_eq!(captures, &vec![IrNode::Local(0)]);
    // The arrow (functions[1]) reads `x` through its environment cell and `y`
    // as a plain argument local.
    let arrow = &program.functions[1];
    assert_eq!(
        arrow.body,
        vec![IrStmt::Return(Some(IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::CellGet(Box::new(IrNode::EnvLoad(0)))),
            rhs: Box::new(IrNode::Local(0)),
        }))]
    );
}

#[test]
fn mutable_capture_reads_and_writes_the_same_cell() {
    // `counter`'s `c` is captured and mutated: its declaration boxes it and
    // both the read and the write in the returned closure go through the cell.
    let program = lower_src("function counter(){ let c=0; return function(){ c=c+1; return c } }")
        .expect("lower");
    let counter = &program.functions[0];
    // `let c = 0` boxes the initializer. With zero parameters the uniform ABI
    // reserves the env slot at local 0 and the argument-count slot at local 1,
    // so the body's `c` is local 2.
    assert_eq!(
        counter.body[0],
        IrStmt::Decl {
            local: 2,
            init: IrNode::CellNew(Box::new(IrNode::Num(0.0))),
        }
    );
    // The closure body writes then reads the captured cell (env slot 0).
    let inner = &program.functions[1];
    assert_eq!(
        inner.body,
        vec![
            IrStmt::Expr(IrNode::CellSet {
                cell: Box::new(IrNode::EnvLoad(0)),
                value: Box::new(IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::CellGet(Box::new(IrNode::EnvLoad(0)))),
                    rhs: Box::new(IrNode::Num(1.0)),
                }),
            }),
            IrStmt::Return(Some(IrNode::CellGet(Box::new(IrNode::EnvLoad(0))))),
        ]
    );
}

#[test]
fn non_capturing_function_has_no_cells_or_captures() {
    // A function whose nested arrow references only its own params captures
    // nothing: no boxing prologue, and the `FuncValue` has empty captures.
    let program = lower_src("let sq = x => x*x; sq(7)").expect("lower");
    let IrStmt::Decl { init, .. } = &program.stmts[0] else {
        unreachable!("expected the `let sq = ..` declaration");
    };
    assert_eq!(
        *init,
        IrNode::FuncValue {
            table_index: 0,
            captures: vec![],
        }
    );
    // The arrow's body is a plain param read — no cell.
    assert_eq!(
        program.functions[0].body,
        vec![IrStmt::Return(Some(IrNode::Binary {
            op: BinOp::Mul,
            lhs: Box::new(IrNode::Local(0)),
            rhs: Box::new(IrNode::Local(0)),
        }))]
    );
}

#[test]
fn nested_function_declaration_lowers_to_bound_func_value() {
    // A nested `function inner(){ .. }` binds `inner` to a function value in
    // the enclosing scope and lifts its body to a table-registered function.
    let program =
        lower_src("function make(){ function inner(){ return 7 } return inner } make()()")
            .expect("lower");
    let make = &program.functions[0];
    // `inner` is declared as a local holding a function value.
    assert!(matches!(
        make.body.first(),
        Some(IrStmt::Decl {
            init: IrNode::FuncValue { .. },
            ..
        })
    ));
    // Two functions exist: `make` (declaration) and the lifted `inner`.
    assert_eq!(program.functions.len(), 2);
}

#[test]
fn rejects_call_to_undeclared_function() {
    assert!(
        lower_src("missing(1)").is_err(),
        "calling an undeclared function is an error"
    );
}

#[test]
fn lowers_while_body_shares_locals() {
    let program = lower_src("let i = 0; while (i < 3) { i = i + 1 }").expect("lower");
    // `i` is the only local; the loop body reuses it rather than allocating.
    assert_eq!(program.locals, 1);
    let IrStmt::While { body, .. } = &program.stmts[1] else {
        unreachable!("expected a while statement");
    };
    assert_eq!(body.len(), 1);
}

#[test]
fn lowers_c_style_for_to_init_and_while() {
    // `for (let i=0; i<3; i=i+1) { }` desugars to `init; while(cond){}` with the
    // update expression carried in the loop's separate `update` slot.
    let program = lower_src("for (let i=0; i<3; i=i+1) { }").expect("lower");
    assert_eq!(program.locals, 1);
    // The init decl is hoisted before the loop.
    assert!(matches!(program.stmts[0], IrStmt::Decl { .. }));
    let IrStmt::While { body, update, .. } = &program.stmts[1] else {
        unreachable!("expected a while statement");
    };
    // The body is empty; the update expression lives in the `update` slot.
    assert_eq!(body.as_slice(), []);
    assert_eq!(update.len(), 1);
    assert!(matches!(update[0], IrStmt::Expr(_)));
}

#[test]
fn lowers_for_empty_cond_to_true() {
    // An omitted condition defaults to the literal `true`.
    let program = lower_src("let i=0; for (; i<3; ) { i = i + 1 }").expect("lower");
    let IrStmt::While { cond, .. } = &program.stmts[1] else {
        unreachable!("expected a while statement");
    };
    // Present here: `i<3`, but with the section omitted it would be `Bool(true)`.
    let program2 = lower_src("let i=0; for (;;) { i = i + 1 }").expect("lower");
    let IrStmt::While { cond: cond2, .. } = &program2.stmts[1] else {
        unreachable!("expected a while statement");
    };
    assert!(matches!(cond, IrNode::Binary { .. }));
    assert!(matches!(cond2, IrNode::Bool(true)));
}

#[test]
fn lowers_switch_with_default_and_break() {
    let program =
        lower_src("let x=1; switch(x){ case 1: case 2: break; default: }").expect("lower");
    // `x` plus the discriminant temp local.
    assert_eq!(program.locals, 2);
    let IrStmt::Switch {
        disc_local, cases, ..
    } = &program.stmts[1]
    else {
        unreachable!("expected a switch statement");
    };
    assert_eq!(*disc_local, 1);
    assert_eq!(cases.len(), 3);
    // Two `case` clauses carry a test; the trailing `default` does not.
    assert!(cases[0].test.is_some());
    assert!(cases[1].test.is_some());
    assert!(cases[2].test.is_none());
    // The first clause is empty (fall-through); the second holds the `break`.
    assert_eq!(cases[0].body.as_slice(), []);
    assert_eq!(cases[1].body, vec![IrStmt::Break(None)]);
}

#[test]
fn lowers_for_in_to_counted_key_loop() {
    // `for (const k in o)` desugars to `iter/len/idx` decls + a `while`; the body's
    // first statement rebinds `k` to the ObjectKey at the current index.
    let program = lower_src("let o={a:1}; for (const k in o) { }").expect("lower");
    let IrStmt::While { body, update, .. } = program.stmts.last().expect("a loop") else {
        unreachable!("expected a while statement");
    };
    // The loop binding is `k = ObjectKey { .. }` as the first body statement.
    let IrStmt::Decl { init, .. } = &body[0] else {
        unreachable!("expected the key-binding decl");
    };
    assert!(matches!(init, IrNode::ObjectKey { .. }));
    // The index increment lives in the `update` slot so a `continue` advances it.
    assert_eq!(update.len(), 1);
}

#[test]
fn rejects_for_in_over_non_enumerable() {
    // A statically-known primitive-scalar operand cannot enumerate keys.
    assert!(
        lower_src("for (const k in 5) { }").is_err(),
        "for-in over a number is rejected"
    );
    assert!(
        lower_src("for (const k in true) { }").is_err(),
        "for-in over a boolean is rejected"
    );
}

#[test]
fn rejects_for_in_destructuring_head() {
    // Only a single identifier head is supported.
    assert!(
        lower_src("for (const [k] in {a:1}) { }").is_err(),
        "for-in with a destructuring head is rejected"
    );
}

#[test]
fn rejects_break_outside_breakable() {
    assert!(
        lower_src("let x=1; break; x").is_err(),
        "a top-level break is illegal"
    );
}

#[test]
fn block_shadow_uses_distinct_local() {
    // Outer `x` is local 0; the inner block's `x` gets a fresh local 1, and
    // the trailing read after the block resolves back to the outer local 0.
    let program = lower_src("let x=1; { let x=2; } x").expect("lower");
    assert_eq!(program.locals, 2);
    let IrStmt::Decl { local: outer, .. } = &program.stmts[0] else {
        unreachable!("expected the outer declaration");
    };
    assert_eq!(*outer, 0);
    let IrStmt::Decl { local: inner, .. } = &program.stmts[1] else {
        unreachable!("expected the inner declaration");
    };
    assert_eq!(*inner, 1);
    // The read after the block sees the outer binding again.
    assert_eq!(program.stmts[2], IrStmt::Expr(IrNode::Local(0)));
}

#[test]
fn block_local_does_not_leak() {
    // A binding declared only inside a block is undeclared after it.
    assert!(
        lower_src("{ let y = 5; } y").is_err(),
        "block-local binding must not leak out"
    );
}

#[test]
fn for_let_binding_does_not_leak() {
    // The `for`-`let` binding is scoped to the loop; referencing it after
    // the loop is a compile error.
    assert!(
        lower_src("for(let i=0;i<3;i=i+1){} i").is_err(),
        "for-let binding must not leak past the loop"
    );
}

#[test]
fn const_plain_reassign_is_compile_error() {
    // Assigning to a `const` binding with `=` is rejected at compile time.
    assert!(
        lower_src("const c = 1; c = 2;").is_err(),
        "reassigning a const must be a compile error"
    );
}

#[test]
fn const_compound_reassign_is_compile_error() {
    // Compound assignment (`+= -= *= /= %=`) to a `const` is rejected.
    for src in [
        "const c = 1; c += 1;",
        "const c = 1; c -= 1;",
        "const c = 1; c *= 2;",
        "const c = 1; c /= 2;",
        "const c = 1; c %= 2;",
    ] {
        assert!(
            lower_src(src).is_err(),
            "compound-assigning a const must be a compile error: {src}"
        );
    }
}

#[test]
fn const_update_reassign_is_compile_error() {
    // `++`/`--` (prefix or postfix) on a `const` is rejected.
    for src in [
        "const c = 1; c++;",
        "const c = 1; c--;",
        "const c = 1; ++c;",
        "const c = 1; --c;",
    ] {
        assert!(
            lower_src(src).is_err(),
            "incrementing/decrementing a const must be a compile error: {src}"
        );
    }
}

#[test]
fn const_shadow_in_inner_block_is_allowed() {
    // Shadowing a `const` with a new binding in an inner block is a fresh
    // declaration, not a reassignment, so it compiles.
    let program = lower_src("const c = 1; { const c = 2; } c").expect("should lower");
    // The trailing read resolves to the outer `const c` (local 0).
    assert_eq!(program.stmts[2], IrStmt::Expr(IrNode::Local(0)));
}

#[test]
fn let_stays_mutable() {
    // A `let` binding may still be reassigned.
    lower_src("let m = 1; m = 2; m").expect("let must remain reassignable");
}

#[test]
fn capturing_top_level_function_reads_module_cell() {
    // A top-level `function` reading a top-level `let` stays a hoisted top-level
    // function called directly; the `let` lives in module cell 0, which the
    // function body reaches without a closure environment.
    let program = lower_src("let n=41; function f(){ return n+1 } f()").expect("lower");
    assert_eq!(program.module_cells, 1);
    assert_eq!(program.functions.len(), 1);
    assert!(
        program
            .stmts
            .iter()
            .any(|s| matches!(s, IrStmt::Expr(IrNode::Call { func: 1, .. }))),
        "the capturing function keeps the direct call"
    );
    assert!(
        matches!(
            program.stmts.first(),
            Some(IrStmt::Decl {
                init: IrNode::CellSet { cell, .. },
                ..
            }) if **cell == IrNode::ModuleCell(0)
        ),
        "`let n` initializes its module cell"
    );
}

#[test]
fn non_capturing_top_level_function_keeps_direct_call() {
    // A top-level `function` that captures nothing keeps the direct-call fast
    // path: no binding in `main`, and the call is a direct `Call` by index.
    let program = lower_src("function f(){ return 7 } f()").expect("lower");
    assert_eq!(
        program.stmts,
        vec![IrStmt::Expr(IrNode::Call {
            func: 1,
            args: vec![],
        })]
    );
}

#[test]
fn capturing_function_callable_from_sibling_and_before_its_binding() {
    // Module cells make a capturing function reachable from a sibling top-level
    // function, and keep it hoisted (an early call hits the runtime TDZ guard).
    lower_src("let n=1; function f(){ return n } function g(){ return f() } g()")
        .expect("sibling call lowers");
    lower_src("f(); let n=41; function f(){ return n }").expect("hoisted call lowers");
}
