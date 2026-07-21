//! These tests drive emitted modules through the `run` stage, so the module is
//! gated on the `native-run` feature; without it there is no runtime to execute
//! the bytes.
#![cfg(feature = "native-run")]

use super::emit;
use crate::ir::{BinOp, IrCase, IrFunction, IrNode, IrProgram, IrStmt};

/// Wrap a single expression into a one-statement program.
fn expr_program(node: IrNode) -> IrProgram {
    IrProgram {
        locals: 0,
        stmts: vec![IrStmt::Expr(node)],
        functions: Vec::new(),
    }
}

fn validate(bytes: &[u8]) {
    let mut validator =
        wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default());
    validator
        .validate_all(bytes)
        .expect("emitted module should be valid wasm");
}

#[test]
fn starts_with_wasm_magic() {
    let bytes = emit(&expr_program(IrNode::Num(42.0)));
    assert_eq!(&bytes[0..4], &[0x00, 0x61, 0x73, 0x6d]);
}

#[test]
fn exports_a_memory_named_memory() {
    // S10.1: every emitted module carries a linear memory exported as
    // "memory" (index 0), which the run stage reads back to resolve pointers.
    let bytes = emit(&expr_program(IrNode::Num(42.0)));
    validate(&bytes);
    let mut found = false;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::ExportSection(reader) = payload.expect("valid payload") {
            for export in reader {
                let export = export.expect("valid export");
                if export.name == "memory" {
                    assert_eq!(export.kind, wasmparser::ExternalKind::Memory);
                    assert_eq!(export.index, 0);
                    found = true;
                }
            }
        }
    }
    assert!(
        found,
        "emitted module should export a memory named \"memory\""
    );
}

#[test]
fn emits_valid_module_for_non_numeric_literals() {
    for node in [
        IrNode::Bool(true),
        IrNode::Bool(false),
        IrNode::Null,
        IrNode::Undefined,
    ] {
        validate(&emit(&expr_program(node)));
    }
}

#[test]
fn compile_and_run_string_literal_end_to_end() {
    // Full JS-source pipeline: a string literal and `typeof` over one.
    let value = crate::run::compile_and_run("\"foo\"").expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Str("foo".to_owned()));
    assert_eq!(crate::run::format_value(&value), "foo");

    let ty = crate::run::compile_and_run("typeof \"hi\"").expect("pipeline should succeed");
    assert_eq!(ty, crate::run::Value::Str("string".to_owned()));
    assert_eq!(crate::run::format_value(&ty), "string");
}

#[test]
fn string_literal_round_trips_to_a_heap_string() {
    // S10.2: a string literal lowers to a heap-pointer jsval backed by a data
    // segment; running the module and decoding against its memory yields the
    // text (unquoted via `format_value`).
    let bytes = emit(&expr_program(IrNode::Str("foo".to_owned())));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    let value = crate::run::decode(bits, &memory);
    assert_eq!(value, crate::run::Value::Str("foo".to_owned()));
    assert_eq!(crate::run::format_value(&value), "foo");
}

#[test]
fn compile_and_run_string_length_end_to_end() {
    // Full JS-source pipeline: `.length` yields the byte count as a number.
    let value = crate::run::compile_and_run("\"foo\".length").expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Number(3.0));
    assert_eq!(crate::run::format_value(&value), "3");

    let empty = crate::run::compile_and_run("\"\".length").expect("pipeline should succeed");
    assert_eq!(empty, crate::run::Value::Number(0.0));
}

#[test]
fn str_len_reads_byte_length_header() {
    // `"foo".length` reads the u32 header (3) and reboxes it as a number.
    let node = IrNode::StrLen(Box::new(IrNode::Str("foo".to_owned())));
    let bytes = emit(&expr_program(node));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Number(3.0)
    );
}

#[test]
fn compile_and_run_string_index_end_to_end() {
    // Full JS-source pipeline: `s[i]` yields a fresh one-char string.
    let value = crate::run::compile_and_run("\"abc\"[1]").expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Str("b".to_owned()));
    assert_eq!(crate::run::format_value(&value), "b");

    let head = crate::run::compile_and_run("\"hello\"[0]").expect("pipeline should succeed");
    assert_eq!(head, crate::run::Value::Str("h".to_owned()));
}

#[test]
fn string_index_result_is_a_real_string() {
    // The one-char result is itself a string: `typeof` and `.length` work.
    let ty = crate::run::compile_and_run("typeof \"abc\"[1]").expect("pipeline should succeed");
    assert_eq!(ty, crate::run::Value::Str("string".to_owned()));

    let len = crate::run::compile_and_run("\"abc\"[1].length").expect("pipeline should succeed");
    assert_eq!(len, crate::run::Value::Number(1.0));
}

#[test]
fn string_index_via_variable() {
    // Indexing a string bound to a variable resolves through the local.
    let value =
        crate::run::compile_and_run("let s = \"xy\"; s[1]").expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Str("y".to_owned()));
}

#[test]
fn str_index_builds_fresh_one_char_string() {
    // `"abc"[2]` allocates a new [len:u32][utf8] blob holding the byte 'c'.
    let node = IrNode::Index {
        obj: Box::new(IrNode::Str("abc".to_owned())),
        index: Box::new(IrNode::Num(2.0)),
    };
    let bytes = emit(&expr_program(node));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Str("c".to_owned())
    );
}

#[test]
fn array_literal_round_trips_to_a_heap_array() {
    // S11.1: `[1, 2, 3]` lowers to a heap array (type byte TYPE_ARRAY); running the
    // module and decoding against its memory yields the element values.
    let node = IrNode::Array(vec![IrNode::Num(1.0), IrNode::Num(2.0), IrNode::Num(3.0)]);
    let bytes = emit(&expr_program(node));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Array(vec![
            crate::run::Value::Number(1.0),
            crate::run::Value::Number(2.0),
            crate::run::Value::Number(3.0),
        ])
    );
}

#[test]
fn empty_array_literal_round_trips() {
    // `[]` (n = 0) allocates a 4-byte blob with a zero len header.
    let bytes = emit(&expr_program(IrNode::Array(Vec::new())));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Array(Vec::new())
    );
}

#[test]
fn array_element_assignment_writes_slot_and_yields_value() {
    // S11.2: `let a = [10, 20]; a[1] = 5; a[0] + a[1]` => 15. The write must
    // land in the existing slot; the read-back sum confirms it.
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Array(vec![IrNode::Num(10.0), IrNode::Num(20.0)]),
            },
            IrStmt::Expr(IrNode::IndexSet {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Num(1.0)),
                value: Box::new(IrNode::Num(5.0)),
            }),
            IrStmt::Expr(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Index {
                    obj: Box::new(IrNode::Local(0)),
                    index: Box::new(IrNode::Num(0.0)),
                }),
                rhs: Box::new(IrNode::Index {
                    obj: Box::new(IrNode::Local(0)),
                    index: Box::new(IrNode::Num(1.0)),
                }),
            }),
        ],
        functions: Vec::new(),
    };
    let bytes = emit(&program);
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Number(15.0)
    );
}

#[test]
fn array_element_assignment_expression_is_the_stored_value() {
    // `a[i] = v` evaluates to `v`: writing 7 into `[0]` yields 7.
    let node = IrNode::IndexSet {
        obj: Box::new(IrNode::Array(vec![IrNode::Num(0.0)])),
        index: Box::new(IrNode::Num(0.0)),
        value: Box::new(IrNode::Num(7.0)),
    };
    let bytes = emit(&expr_program(node));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Number(7.0)
    );
}

#[test]
fn object_property_assignment_static_member_updates_in_place() {
    // S11.4: `o.x = 42` scans for key "x" and stores into its value slot.
    let value = crate::run::compile_and_run("let o = {x: 10}; o.x = 42; o.x")
        .expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Number(42.0));
}

#[test]
fn object_property_assignment_computed_member_updates_in_place() {
    // `o["a"] = 7` is the computed form of the same key-scan store.
    let value = crate::run::compile_and_run("let o = {a: 1}; o[\"a\"] = 7; o.a")
        .expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Number(7.0));
}

#[test]
fn object_property_assignment_expression_is_the_stored_value() {
    // `o.a = v` evaluates to `v` (JS semantics), independent of the store.
    let value = crate::run::compile_and_run("let o = {a: 0}; let x = (o.a = 9); x")
        .expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Number(9.0));
}

#[test]
fn object_property_assignment_missing_key_is_a_noop() {
    // Existing keys only: `o.b = 5` on an object without `b` writes nothing
    // and must not corrupt the existing `a` entry.
    let value = crate::run::compile_and_run("let o = {a: 1}; o.b = 5; o.a")
        .expect("pipeline should succeed");
    assert_eq!(value, crate::run::Value::Number(1.0));
}

#[test]
fn object_property_assignment_via_indexset_ir_updates_slot() {
    // Direct IR: `IndexSet` on an object pointer dispatches to `__obj_set`,
    // stores 42 into the "x" entry, and yields the stored value.
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Object(vec![("x".to_owned(), IrNode::Num(10.0))]),
            },
            IrStmt::Expr(IrNode::IndexSet {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("x".to_owned())),
                value: Box::new(IrNode::Num(42.0)),
            }),
            IrStmt::Expr(IrNode::Index {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("x".to_owned())),
            }),
        ],
        functions: Vec::new(),
    };
    let bytes = emit(&program);
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Number(42.0)
    );
}

#[test]
fn typeof_string_literal_reports_string() {
    // `typeof "hi"` must map the pointer tag (0b101) to index 4 ("string").
    let bytes = emit(&expr_program(IrNode::TypeOf(Box::new(IrNode::Str(
        "hi".to_owned(),
    )))));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Str("string".to_owned())
    );
}

#[test]
fn string_literal_emits_data_segment_and_advances_heap_ptr() {
    // The literal "foo" is an 11-byte block ([type:u8][pad×3][len:u32][utf8],
    // HEADER_SIZE=4) packed from base offset 8, so `__heap_ptr` must initialize
    // to 8 + 11 = 19, past it.
    let bytes = emit(&expr_program(IrNode::Str("foo".to_owned())));
    validate(&bytes);
    let mut data_segments = 0;
    let mut heap_ptr_init = None;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        match payload.expect("valid payload") {
            wasmparser::Payload::DataSection(reader) => {
                data_segments = reader.count();
            }
            wasmparser::Payload::GlobalSection(reader) => {
                // The heap pointer is the FIRST global; the S15.1
                // exception-unwinding globals follow it and are ignored here.
                let heap_ptr = reader
                    .into_iter()
                    .next()
                    .expect("at least the heap-pointer global")
                    .expect("valid global");
                let mut ops = heap_ptr.init_expr.get_operators_reader();
                if let wasmparser::Operator::I32Const { value } = ops.read().expect("init operator")
                {
                    heap_ptr_init = Some(value);
                }
            }
            _ => {}
        }
    }
    assert_eq!(data_segments, 1, "one data segment for the single literal");
    assert_eq!(heap_ptr_init, Some(19));
}

#[test]
fn no_literals_keeps_heap_ptr_at_base() {
    // With no string literals, the bump allocator still starts at HEAP_BASE.
    // The heap pointer is the FIRST global; the S15.1 exception-unwinding
    // globals ($exc_pending = 0, $exc_value = 0) follow it and are ignored here.
    let bytes = emit(&expr_program(IrNode::Num(42.0)));
    validate(&bytes);
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::GlobalSection(reader) = payload.expect("valid payload") {
            let heap_ptr = reader
                .into_iter()
                .next()
                .expect("at least the heap-pointer global")
                .expect("valid global");
            let mut ops = heap_ptr.init_expr.get_operators_reader();
            if let wasmparser::Operator::I32Const { value } = ops.read().expect("init operator") {
                assert_eq!(value, 8);
            }
        }
    }
}

#[test]
fn emits_valid_module_for_typeof() {
    // `typeof` over each in-scope literal type must stay valid wasm (it uses
    // the scratch local, select, and i64 bit ops).
    for node in [
        IrNode::Num(42.0),
        IrNode::Bool(true),
        IrNode::Null,
        IrNode::Undefined,
    ] {
        validate(&emit(&expr_program(IrNode::TypeOf(Box::new(node)))));
    }
}

#[test]
fn emits_valid_module() {
    let bytes = emit(&expr_program(IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Num(40.0)),
        rhs: Box::new(IrNode::Num(2.0)),
    }));
    validate(&bytes);
}

#[test]
fn emits_valid_module_with_locals() {
    // `let x = 40; x + 2` — a declaration plus a reference over one local.
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Num(40.0),
            },
            IrStmt::Expr(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(0)),
                rhs: Box::new(IrNode::Num(2.0)),
            }),
        ],
        functions: Vec::new(),
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_while_loop() {
    // `let i = 0; while (i < 3) { i = i + 1 } i`
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Num(0.0),
            },
            IrStmt::While {
                cond: IrNode::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(IrNode::Local(0)),
                    rhs: Box::new(IrNode::Num(3.0)),
                },
                body: vec![IrStmt::Expr(IrNode::Assign {
                    local: 0,
                    value: Box::new(IrNode::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(IrNode::Local(0)),
                        rhs: Box::new(IrNode::Num(1.0)),
                    }),
                })],
                update: Vec::new(),
                label: None,
            },
            IrStmt::Expr(IrNode::Local(0)),
        ],
        functions: Vec::new(),
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_switch() {
    // `let x = 2; let r = 0;
    //  switch (x) { case 1: r = 10; break; default: r = 99; case 2: r = 42; break } r`
    // Covers a default clause sitting between cases plus a `break` inside a body.
    let assign = |local, num| {
        IrStmt::Expr(IrNode::Assign {
            local,
            value: Box::new(IrNode::Num(num)),
        })
    };
    let program = IrProgram {
        locals: 3, // x, r, and the discriminant temp
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Num(2.0),
            },
            IrStmt::Decl {
                local: 1,
                init: IrNode::Num(0.0),
            },
            IrStmt::Switch {
                disc_local: 2,
                disc: IrNode::Local(0),
                cases: vec![
                    IrCase {
                        test: Some(IrNode::Num(1.0)),
                        body: vec![assign(1, 10.0), IrStmt::Break(None)],
                    },
                    IrCase {
                        test: None,
                        body: vec![assign(1, 99.0)],
                    },
                    IrCase {
                        test: Some(IrNode::Num(2.0)),
                        body: vec![assign(1, 42.0), IrStmt::Break(None)],
                    },
                ],
            },
            IrStmt::Expr(IrNode::Local(1)),
        ],
        functions: Vec::new(),
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_boxed_condition() {
    // S9.4: `if (true) { 1 } else { 2 }` — the generalized `emit_cond` must
    // stay valid wasm when the condition is a boxed (non-number) jsval.
    let program = IrProgram {
        locals: 0,
        stmts: vec![IrStmt::If {
            cond: IrNode::Bool(true),
            then_body: vec![IrStmt::Expr(IrNode::Num(1.0))],
            else_body: vec![IrStmt::Expr(IrNode::Num(2.0))],
        }],
        functions: Vec::new(),
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_strict_equality() {
    // S9.5: `1 === 2` and `null !== undefined` — the strict-equality path uses
    // both scratch locals, select, and mixed f64/i64 bit ops.
    for op in [BinOp::Eq, BinOp::Ne] {
        let program = expr_program(IrNode::Binary {
            op,
            lhs: Box::new(IrNode::Num(1.0)),
            rhs: Box::new(IrNode::Null),
        });
        validate(&emit(&program));
    }
}

#[test]
fn emits_valid_module_with_ternary() {
    // S9.5: `true ? 42 : 0` — the ternary lowers to a typed `if`/`else`.
    let program = expr_program(IrNode::Ternary {
        cond: Box::new(IrNode::Bool(true)),
        then_branch: Box::new(IrNode::Num(42.0)),
        else_branch: Box::new(IrNode::Num(0.0)),
    });
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_string_concatenation() {
    // S10.4: `"foo" + "bar"` — the `+` dispatch uses both scratch locals, a
    // typed `if`, and a call into the appended `__concat` helper.
    let bytes = emit(&expr_program(IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Str("foo".to_owned())),
        rhs: Box::new(IrNode::Str("bar".to_owned())),
    }));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Str("foobar".to_owned())
    );
}

#[test]
fn heap_blocks_write_the_type_byte_header() {
    // S13.1: every heap block begins with its TYPE BYTE at the base offset. Run
    // a literal, take the low 32 bits of the returned jsval as the block base,
    // and assert the byte there is the expected kind (0 string / 1 array / 2
    // object) with the three padding bytes zeroed.
    for (src, expected_type) in [("\"hi\"", 0u8), ("[7, 8]", 1u8), ("let o = {a: 1}; o", 2u8)] {
        let program =
            crate::codegen::lower(&crate::frontend::parse(src).expect("parse")).expect("lower");
        let bytes = emit(&program);
        validate(&bytes);
        let (bits, memory) = crate::run::run(&bytes).expect("module should run");
        // A heap jsval carries tag 0b101 and the block base in its low 32 bits.
        assert_eq!((bits >> 48) & 0b111, 0b101, "{src} should be a heap jsval");
        let base = (bits & 0xFFFF_FFFF) as usize;
        assert_eq!(memory[base], expected_type, "type byte for {src}");
        assert_eq!(
            &memory[base + 1..base + 4],
            &[0, 0, 0],
            "header padding for {src}"
        );
    }
}

#[test]
fn emits_valid_module_with_function_and_call() {
    // `function add(a, b) { return a + b } add(40, 2)` under the uniform ABI
    // (env-last): locals 0/1 are the argument slots, local 2 is the env slot,
    // local 3 is the argument-count slot (P1.4).
    let program = IrProgram {
        locals: 0,
        stmts: vec![IrStmt::Expr(IrNode::Call {
            func: 1,
            args: vec![IrNode::Num(40.0), IrNode::Num(2.0)],
        })],
        functions: vec![IrFunction {
            params: 4,
            locals: 4,
            body: vec![IrStmt::Return(Some(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(0)),
                rhs: Box::new(IrNode::Local(1)),
            }))],
            machine: None,
        }],
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_function_value_and_indirect_call() {
    // `function id(x){ return x } let f = id; f(42)` — a function value plus a
    // `call_indirect` through the funcref table. `id` is wasm function 1, so
    // its table index is 0. Under the uniform ABI (env-last) its arity is 1
    // (params = 1 arg slot + env + argc = 3; `x` is local 0, env is local 1,
    // argc is local 2).
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::FuncValue {
                    table_index: 0,
                    captures: vec![],
                },
            },
            IrStmt::Expr(IrNode::CallIndirect {
                callee: Box::new(IrNode::Local(0)),
                args: vec![IrNode::Num(42.0)],
            }),
        ],
        functions: vec![IrFunction {
            params: 3,
            locals: 3,
            body: vec![IrStmt::Return(Some(IrNode::Local(0)))],
            machine: None,
        }],
    };
    let bytes = emit(&program);
    validate(&bytes);
    // The module must carry a funcref table for the indirect call.
    let mut has_table = false;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::TableSection(reader) = payload.expect("valid payload") {
            assert_eq!(reader.count(), 1, "exactly one funcref table");
            has_table = true;
        }
    }
    assert!(
        has_table,
        "a function-value module must emit a table section"
    );
}

#[test]
fn emits_valid_closure_module_with_cells_and_env() {
    // A closure that captures and returns an enclosing binding: the emitted
    // module (boxed cell `CellNew`/`CellGet`, environment block, and
    // env-slot function value) must be a valid wasm module.
    let program = crate::codegen::lower(
        &crate::frontend::parse("function adder(x){ return y => x + y } adder(5)(37)")
            .expect("parse"),
    )
    .expect("lower");
    let bytes = emit(&program);
    validate(&bytes);
    // It computes the closure result end to end.
    let (bits, _memory) = crate::run::run(&bytes).expect("module should run");
    assert!(
        (f64::from_bits(bits) - 42.0).abs() < 1e-9,
        "closure result should be 42"
    );
}
