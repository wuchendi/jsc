//! These tests drive emitted modules through the `run` stage, so the module is
//! gated on the `native-run` feature; without it there is no runtime to execute
//! the bytes.
#![cfg(feature = "native-run")]

use super::{builtin_count, collect_strings, emit, intern_table, realm};
use crate::abi;
use crate::ir::{BinOp, CalleeError, IrCase, IrFunction, IrNode, IrProgram, IrStmt, PropKey};

/// Wrap a single expression into a one-statement program.
fn expr_program(node: IrNode) -> IrProgram {
    IrProgram {
        locals: 0,
        stmts: vec![IrStmt::Expr(node)],
        functions: Vec::new(),
        module_cells: 0,
        arity: 0,
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
    // Every emitted module carries a linear memory exported as
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
    // A string literal lowers to a heap-pointer jsval backed by a data
    // segment; running the module and decoding against its memory yields the
    // text (unquoted via `format_value`).
    let bytes = emit(&expr_program(IrNode::Str("foo".into())));
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
    let node = IrNode::StrLen(Box::new(IrNode::Str("foo".into())));
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
        obj: Box::new(IrNode::Str("abc".into())),
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
    // `[1, 2, 3]` lowers to a heap array (type byte TYPE_ARRAY); running the
    // module and decoding against its memory yields the element values.
    let node = IrNode::Array(vec![IrNode::Num(1.0), IrNode::Num(2.0), IrNode::Num(3.0)]);
    let bytes = emit(&expr_program(node));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Array(
            vec![
                crate::run::Value::Number(1.0),
                crate::run::Value::Number(2.0),
                crate::run::Value::Number(3.0),
            ],
            Vec::new()
        )
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
        crate::run::Value::Array(Vec::new(), Vec::new())
    );
}

#[test]
fn array_element_assignment_writes_slot_and_yields_value() {
    // `let a = [10, 20]; a[1] = 5; a[0] + a[1]` => 15. The write must
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
        module_cells: 0,
        arity: 0,
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
    // `o.x = 42` scans for key "x" and stores into its value slot.
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
fn object_property_assignment_missing_key_adds_property() {
    // `o.b = 5` on an object without `b` adds an own data property after `a`.
    let value =
        crate::run::compile_and_run("let o = {a: 1}; o.b = 5; o").expect("pipeline should succeed");
    assert_eq!(
        value,
        crate::run::Value::Object(vec![
            ("a".to_owned(), crate::run::Value::Number(1.0)),
            ("b".to_owned(), crate::run::Value::Number(5.0)),
        ])
    );
}

#[test]
fn object_property_assignment_via_indexset_ir_updates_slot() {
    // Direct IR: `IndexSet` on an object pointer dispatches to `__set`,
    // stores 42 into the "x" entry, and yields the stored value.
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Object {
                    entries: vec![(PropKey::Static("x".into()), IrNode::Num(10.0))],
                    proto: None,
                    class: crate::abi::CLASS_ORDINARY,
                    reserve: 0,
                },
            },
            IrStmt::Expr(IrNode::IndexSet {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("x".into())),
                value: Box::new(IrNode::Num(42.0)),
            }),
            IrStmt::Expr(IrNode::Index {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("x".into())),
            }),
        ],
        functions: Vec::new(),
        module_cells: 0,
        arity: 0,
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
fn object_property_assignment_via_indexset_ir_missing_key_adds() {
    // Direct IR: an `IndexSet` whose key the object lacks appends it (`__set`
    // adds an own data property), so a later read finds it.
    let program = IrProgram {
        locals: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::Object {
                    entries: vec![(PropKey::Static("x".into()), IrNode::Num(10.0))],
                    proto: None,
                    class: crate::abi::CLASS_ORDINARY,
                    reserve: 0,
                },
            },
            IrStmt::Expr(IrNode::IndexSet {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("y".into())),
                value: Box::new(IrNode::Num(42.0)),
            }),
            IrStmt::Expr(IrNode::Index {
                obj: Box::new(IrNode::Local(0)),
                index: Box::new(IrNode::Str("y".into())),
            }),
        ],
        functions: Vec::new(),
        module_cells: 0,
        arity: 0,
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
        "hi".into(),
    )))));
    validate(&bytes);
    let (bits, memory) = crate::run::run(&bytes).expect("module should run");
    assert_eq!(
        crate::run::decode(bits, &memory),
        crate::run::Value::Str("string".to_owned())
    );
}

/// The data-segment count and the initial `__heap_ptr` of `bytes`.
fn data_layout(bytes: &[u8]) -> (u32, Option<i32>) {
    let mut data_segments = 0;
    let mut heap_ptr_init = None;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.expect("valid payload") {
            wasmparser::Payload::DataSection(reader) => {
                data_segments = reader.count();
            }
            wasmparser::Payload::GlobalSection(reader) => {
                // The heap pointer is the FIRST global; the
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
    (data_segments, heap_ptr_init)
}

#[test]
fn string_literal_emits_data_segment_and_advances_heap_ptr() {
    // The literal "foo" (11 bytes, padded to 16) is one more string blob
    // segment ahead of the function data, the realm image, and the intern table.
    let base = emit(&expr_program(IrNode::Num(42.0)));
    let bytes = emit(&expr_program(IrNode::Str("foo".into())));
    validate(&bytes);
    let (segments, heap_ptr) = data_layout(&bytes);
    let (base_segments, base_heap_ptr) = data_layout(&base);
    assert_eq!(segments, base_segments + 1, "one more string blob");
    assert_eq!(heap_ptr, base_heap_ptr.map(|p| p + 16));
}

#[test]
fn no_literals_starts_heap_after_the_intern_table() {
    // With no string literals, the static strings (the always-interned
    // "length", the realm's and the builtins' strings), the `__fn_info` table
    // (the two settlers and the builtins), the realm image and the intern table
    // precede the bump allocator, in that order.
    let program = expr_program(IrNode::Num(42.0));
    let bytes = emit(&program);
    validate(&bytes);
    let (offsets, strings_end) = collect_strings(&program);
    let info = abi::FN_INFO_ENTRY_SIZE.cast_signed() * (2 + builtin_count());
    let intern_base = strings_end + info + realm::image_size();
    let cap = intern_table(&offsets, intern_base).cap;
    assert_eq!(data_layout(&bytes).1, Some(intern_base + 4 * cap));
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
        module_cells: 0,
        arity: 0,
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
        module_cells: 0,
        arity: 0,
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
        module_cells: 0,
        arity: 0,
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_boxed_condition() {
    // `if (true) { 1 } else { 2 }` — the generalized `emit_cond` must
    // stay valid wasm when the condition is a boxed (non-number) jsval.
    let program = IrProgram {
        locals: 0,
        stmts: vec![IrStmt::If {
            cond: IrNode::Bool(true),
            then_body: vec![IrStmt::Expr(IrNode::Num(1.0))],
            else_body: vec![IrStmt::Expr(IrNode::Num(2.0))],
        }],
        functions: Vec::new(),
        module_cells: 0,
        arity: 0,
    };
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_strict_equality() {
    // `1 === 2` and `null !== undefined` — the strict-equality path uses
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
    // `true ? 42 : 0` — the ternary lowers to a typed `if`/`else`.
    let program = expr_program(IrNode::Ternary {
        cond: Box::new(IrNode::Bool(true)),
        then_branch: Box::new(IrNode::Num(42.0)),
        else_branch: Box::new(IrNode::Num(0.0)),
    });
    validate(&emit(&program));
}

#[test]
fn emits_valid_module_with_string_concatenation() {
    // `"foo" + "bar"` — the `+` dispatch uses both scratch locals, a
    // typed `if`, and a call into the appended `__concat` helper.
    let bytes = emit(&expr_program(IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Str("foo".into())),
        rhs: Box::new(IrNode::Str("bar".into())),
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
    // Every heap block begins with its TYPE BYTE at the base offset. Run
    // a literal, take the low 32 bits of the returned jsval as the block base,
    // and assert the byte there is the expected kind (0 string / 1 array / 2
    // object) followed by the `gc`/`flags`/`class` bytes: zero, except a static
    // string's `flags`, which carry `INTERNED`.
    for (src, expected_type, flags) in [
        ("\"hi\"", 0u8, crate::abi::STR_FLAG_INTERNED),
        ("[7, 8]", 1u8, 0),
        ("let o = {a: 1}; o", 2u8, 0),
    ] {
        let program = crate::codegen::lower(
            &crate::frontend::parse(src, crate::frontend::Dialect::Js).expect("parse"),
        )
        .expect("lower");
        let bytes = emit(&program);
        validate(&bytes);
        let (bits, memory) = crate::run::run(&bytes).expect("module should run");
        // A heap jsval carries tag 0b101 and the block base in its low 32 bits.
        assert_eq!((bits >> 48) & 0b111, 0b101, "{src} should be a heap jsval");
        let base = (bits & 0xFFFF_FFFF) as usize;
        assert_eq!(memory[base], expected_type, "type byte for {src}");
        assert_eq!(
            &memory[base + 1..base + 4],
            &[0, flags, 0],
            "header bytes for {src}"
        );
    }
}

#[test]
fn emits_valid_module_with_function_and_call() {
    // `function add(a, b) { return a + b } add(40, 2)` under the uniform ABI
    // (env-last): locals 0/1 are the argument slots, local 2 is the env slot,
    // local 3 is the argument-count slot.
    let program = IrProgram {
        locals: 0,
        module_cells: 0,
        arity: 2,
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
            info: crate::ir::FnInfo::default(),
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
        module_cells: 0,
        arity: 1,
        stmts: vec![
            IrStmt::Decl {
                local: 0,
                init: IrNode::FuncValue {
                    table_index: 0,
                    captures: vec![],
                    kind: crate::ir::FnKind::Plain,
                },
            },
            IrStmt::Expr(IrNode::CallIndirect {
                callee: Box::new(IrNode::Local(0)),
                args: vec![IrNode::Num(42.0)],
                not_callable: CalleeError::not_a_function("f"),
            }),
        ],
        functions: vec![IrFunction {
            params: 3,
            locals: 3,
            body: vec![IrStmt::Return(Some(IrNode::Local(0)))],
            machine: None,
            info: crate::ir::FnInfo::default(),
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
        &crate::frontend::parse(
            "function adder(x){ return y => x + y } adder(5)(37)",
            crate::frontend::Dialect::Js,
        )
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

/// Lower and emit JavaScript `src`.
fn emit_js(src: &str) -> Vec<u8> {
    let program = crate::codegen::lower(
        &crate::frontend::parse(src, crate::frontend::Dialect::Js).expect("parse"),
    )
    .expect("lower");
    emit(&program)
}

#[test]
fn static_string_blobs_are_8_aligned() {
    // Odd-length literals are each padded to 8, so every blob base (and the
    // heap start after them) is 8-aligned.
    let program = crate::codegen::lower(
        &crate::frontend::parse(
            "let a = 'a'; let b = 'abc'; let c = 'abcdefghi'; a + b + c",
            crate::frontend::Dialect::Js,
        )
        .expect("parse"),
    )
    .expect("lower");
    let (offsets, heap_start) = super::sections::collect_strings(&program);
    assert!(offsets.len() >= 3, "three literals interned");
    for (text, offset) in offsets.iter() {
        assert_eq!(offset % 8, 0, "blob {text:?} at {offset} is not 8-aligned");
    }
    assert_eq!(
        heap_start % 8,
        0,
        "heap start {heap_start} is not 8-aligned"
    );
}

#[test]
fn every_block_is_8_aligned_with_zero_header_bytes() {
    // Guest-allocated (concat, array, object, function, Map, Date, generator)
    // and host-allocated (String(n), string/array methods, promises) blocks
    // all start 8-aligned with header bytes 1..3 (`gc`, `flags`, `class`) zero,
    // except a fresh arrow's `flags`, which is `LAZY`.
    for src in [
        "'x' + 1",
        "let n = 3; [1, n]",
        "let o = {a: 1, bc: 2}; o",
        "let f = (x) => x; f",
        "new Map()",
        "new Date(0)",
        "function* g() { yield 1 } g()",
        "String(12345)",
        "'abc'.toUpperCase()",
        "'abcde'.slice(1)",
        "[1, 2, 3].slice(1)",
        "'a,b'.split(',')",
        "Promise.resolve(1)",
        "new Promise((r) => r(1))",
    ] {
        // Allocate an odd-sized block first so an unrounded allocator would
        // misalign the block under test.
        let bytes = emit_js(&format!("let pad = 'ab' + 'c'; {src}"));
        validate(&bytes);
        let (bits, memory) = crate::run::run(&bytes).expect("module should run");
        assert_eq!((bits >> 48) & 0b111, 0b101, "{src} should be a heap jsval");
        let base = (bits & 0xFFFF_FFFF) as usize;
        assert_eq!(base % 8, 0, "{src}: block base {base} is not 8-aligned");
        for (name, offset) in [
            ("gc", crate::abi::HEADER_GC_OFFSET),
            ("flags", crate::abi::HEADER_FLAGS_OFFSET),
            ("class", crate::abi::HEADER_CLASS_OFFSET),
        ] {
            let want = if name == "flags" && memory[base] == crate::abi::TYPE_FUNCTION {
                crate::abi::FUNC_FLAG_LAZY
            } else {
                0
            };
            assert_eq!(
                memory[base + offset as usize],
                want,
                "{src}: header {name} byte"
            );
        }
    }
}

#[test]
fn exported_constructors_build_aligned_zeroed_blocks() {
    use wasmtime::{Engine, Linker, Module, Store};
    let bytes = emit_js("let s = 'odd'; s");
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).expect("compile");
    let mut linker = Linker::new(&engine);
    linker
        .define_unknown_imports_as_traps(&module)
        .expect("stub imports");
    let mut store = Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate");
    let memory = instance.get_memory(&mut store, "memory").expect("memory");
    let heap_ptr = instance
        .get_global(&mut store, crate::abi::HEAP_PTR_EXPORT_NAME)
        .expect("heap ptr");
    let ptr = |store: &mut Store<()>| heap_ptr.get(&mut *store).i32().expect("i32");
    assert_eq!(ptr(&mut store) % 8, 0, "heap start is 8-aligned");
    // Dirty the free region so the constructors must WRITE the zero bytes.
    let start = usize::try_from(ptr(&mut store)).expect("ptr");
    memory.data_mut(&mut store)[start..start + 256].fill(0xFF);

    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, crate::abi::ALLOC_EXPORT_NAME)
        .expect("__alloc export");
    let a = alloc.call(&mut store, 5).expect("alloc");
    assert_eq!(a % 8, 0);
    assert_eq!(ptr(&mut store) - a, 8, "__alloc(5) rounds up to 8");

    let new_string = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, crate::abi::NEW_STRING_EXPORT_NAME)
        .expect("__new_string export");
    let s = new_string.call(&mut store, (3, 0)).expect("new string");
    assert_eq!(s % 8, 0);
    let mem = memory.data(&store);
    let s = usize::try_from(s).expect("base");
    assert_eq!(&mem[s..s + 4], &[crate::abi::TYPE_STRING, 0, 0, 0]);
    assert_eq!(&mem[s + 4..s + 8], &3u32.to_le_bytes());
    // A two-byte string sets the flag; its 3 units take 6 data bytes.
    let t = new_string
        .call(&mut store, (3, 1))
        .expect("new two-byte string");
    assert_eq!(
        t - i32::try_from(s).expect("base"),
        16,
        "8 + 3 rounds up to 16"
    );
    let mem = memory.data(&store);
    let t = usize::try_from(t).expect("base");
    assert_eq!(
        &mem[t..t + 4],
        &[crate::abi::TYPE_STRING, 0, crate::abi::STR_FLAG_TWO_BYTE, 0]
    );
    assert_eq!(&mem[t + 4..t + 8], &3u32.to_le_bytes());

    let new_array = instance
        .get_typed_func::<i32, i32>(&mut store, crate::abi::NEW_ARRAY_EXPORT_NAME)
        .expect("__new_array export");
    let arr = new_array.call(&mut store, 2).expect("new array");
    assert_eq!(arr % 8, 0);
    let mem = memory.data(&store);
    let arr = usize::try_from(arr).expect("base");
    assert_eq!(&mem[arr..arr + 4], &[crate::abi::TYPE_ARRAY, 0, 0, 0]);
    assert_eq!(&mem[arr + 4..arr + 8], &0u32.to_le_bytes(), "props");
    // `proto` = `%Array.prototype%`.
    assert_eq!(
        mem[arr + 14..arr + 16],
        [0xFD, 0x7F],
        "proto is a heap pointer"
    );
    assert_eq!(&mem[arr + 16..arr + 20], &2u32.to_le_bytes(), "len");
    let elems = u32::from_le_bytes(mem[arr + 20..arr + 24].try_into().expect("elems"));
    let elems = usize::try_from(elems).expect("elems");
    assert_eq!(elems % 8, 0);
    assert_eq!(&mem[elems..elems + 4], &[crate::abi::TYPE_ELEMS, 0, 0, 0]);
    assert_eq!(&mem[elems + 4..elems + 8], &2u32.to_le_bytes(), "cap");
    for i in 0..2 {
        let at = elems + 8 + 8 * i;
        assert_eq!(&mem[at..at + 8], &crate::abi::UNDEFINED.to_le_bytes());
    }
    let empty = new_array.call(&mut store, 0).expect("empty array");
    let mem = memory.data(&store);
    let empty = usize::try_from(empty).expect("base");
    assert_eq!(&mem[empty + 16..empty + 24], &[0; 8], "len 0, no store");

    let new_promise = instance
        .get_typed_func::<(), i32>(&mut store, crate::abi::NEW_PROMISE_EXPORT_NAME)
        .expect("__new_promise export");
    let p = new_promise.call(&mut store, ()).expect("new promise");
    assert_eq!(p % 8, 0);
    let mem = memory.data(&store);
    let p = usize::try_from(p).expect("base");
    assert_eq!(&mem[p..p + 4], &[crate::abi::TYPE_PROMISE, 0, 0, 0]);
    assert_eq!(&mem[p + 4..p + 8], &0u32.to_le_bytes(), "props");
    // `proto` = `%Promise.prototype%`.
    assert_eq!(mem[p + 14..p + 16], [0xFD, 0x7F], "proto is a heap pointer");
    assert_eq!(
        &mem[p + 16..p + 20],
        &crate::abi::PROMISE_PENDING.to_le_bytes()
    );
    assert_eq!(&mem[p + 24..p + 32], &crate::abi::UNDEFINED.to_le_bytes());
}

#[test]
#[allow(clippy::too_many_lines)] // one straight-line layout walk per kind
fn object_family_blocks_carry_the_prefix_and_payload_offsets() {
    use crate::abi::{
        ARRAY_ELEMS_OFFSET, ARRAY_LEN_OFFSET, DATE_MS_OFFSET, ELEMS_CAP_OFFSET, ELEMS_DATA_OFFSET,
        FUNC_ENV_OFFSET, FUNC_FLAG_CONSTRUCTOR, FUNC_FLAG_LAZY, FUNC_INDEX_OFFSET, GEN_DONE_OFFSET,
        GEN_ENV_OFFSET, GEN_STATE_OFFSET, MAP_ENTRIES_OFFSET, MAP_SIZE_OFFSET, OBJ_PROPS_OFFSET,
        OBJ_PROTO_OFFSET, PROMISE_FULFILLED, PROMISE_STATE_OFFSET, PROMISE_VALUE_OFFSET,
        TYPE_ARRAY, TYPE_DATE, TYPE_ELEMS, TYPE_FUNCTION, TYPE_GENERATOR, TYPE_MAP, TYPE_OBJECT,
        TYPE_PROMISE, TYPE_SET, UNDEFINED,
    };
    let u32_at = |mem: &[u8], at: u32| {
        let at = at as usize;
        u32::from_le_bytes(mem[at..at + 4].try_into().expect("u32"))
    };
    let u64_at = |mem: &[u8], at: u32| {
        let at = at as usize;
        u64::from_le_bytes(mem[at..at + 8].try_into().expect("u64"))
    };
    // `%Function.prototype%`: a non-lazy `TYPE_FUNCTION` block.
    let function_proto = |mem: &[u8], bits: u64| {
        assert_eq!(
            (bits >> 48) & 0b111,
            0b101,
            "function proto is a heap block"
        );
        let at = usize::try_from(bits & 0xFFFF_FFFF).expect("base");
        assert_eq!(
            &mem[at..at + 4],
            &[TYPE_FUNCTION, 0, 0, 0],
            "%Function.prototype%"
        );
    };
    let heap = |src: &str, kind: u8, flags: u8| {
        let bytes = emit_js(src);
        validate(&bytes);
        let (bits, memory) = crate::run::run(&bytes).expect("module should run");
        assert_eq!((bits >> 48) & 0b111, 0b101, "{src} should be a heap jsval");
        let base = u32::try_from(bits & 0xFFFF_FFFF).expect("base");
        assert_eq!(base % 8, 0, "{src}: 8-aligned");
        let at = base as usize;
        assert_eq!(&memory[at..at + 4], &[kind, 0, flags, 0], "{src}: header");
        assert_eq!(u32_at(&memory, base + OBJ_PROPS_OFFSET), 0, "{src}: props");
        let proto = u64_at(&memory, base + OBJ_PROTO_OFFSET);
        if kind == TYPE_FUNCTION {
            function_proto(&memory, proto);
        } else {
            // Arrays, Map/Set, promises and dates inherit from their realm
            // prototype, generators from %Object.prototype%: an ordinary object.
            assert_eq!((proto >> 48) & 0b111, 0b101, "{src}: proto is a heap block");
            let at = usize::try_from(proto & 0xFFFF_FFFF).expect("base");
            assert_eq!(memory[at], TYPE_OBJECT, "{src}: proto");
        }
        (base, memory)
    };

    let (func, mem) = heap(
        "function f(x) { return x } f",
        TYPE_FUNCTION,
        FUNC_FLAG_LAZY | FUNC_FLAG_CONSTRUCTOR,
    );
    assert_eq!(u32_at(&mem, func + FUNC_INDEX_OFFSET), 0, "func_idx");
    assert_eq!(u32_at(&mem, func + FUNC_INDEX_OFFSET + 4), 0, "reserved");
    assert_eq!(u64_at(&mem, func + FUNC_ENV_OFFSET), UNDEFINED, "env");

    let (closure, mem) = heap(
        "let n = 1; let g = () => n; g",
        TYPE_FUNCTION,
        FUNC_FLAG_LAZY,
    );
    let env = u64_at(&mem, closure + FUNC_ENV_OFFSET);
    assert_eq!((env >> 48) & 0b111, 0b101, "closure env is a heap block");

    for (src, kind, size) in [
        (
            "let m = new Map(); m.set(1, 'a'); m.set(2, 'b'); m",
            TYPE_MAP,
            2,
        ),
        ("let s = new Set(); s.add(1); s", TYPE_SET, 1),
    ] {
        let (handle, mem) = heap(src, kind, 0);
        assert_eq!(u32_at(&mem, handle + MAP_SIZE_OFFSET), size, "{src}: size");
        let entries = u64_at(&mem, handle + MAP_ENTRIES_OFFSET);
        let arr = u32::try_from(entries & 0xFFFF_FFFF).expect("entries");
        assert_eq!(mem[arr as usize], TYPE_ARRAY, "{src}: entries backing");
        assert_eq!(
            u32_at(&mem, arr + ARRAY_LEN_OFFSET),
            2 * size,
            "{src}: pairs"
        );
    }

    // An array literal owns a `cap = len` store; pushes grow it by doubling
    // from 4, and the handle never moves.
    let (lit, mem) = heap("[1, 2, 3]", TYPE_ARRAY, 0);
    assert_eq!(u32_at(&mem, lit + ARRAY_LEN_OFFSET), 3, "literal len");
    let store = u32_at(&mem, lit + ARRAY_ELEMS_OFFSET);
    assert_eq!(mem[store as usize], TYPE_ELEMS, "literal store");
    assert_eq!(u32_at(&mem, store + ELEMS_CAP_OFFSET), 3, "literal cap");
    assert_eq!(
        u64_at(&mem, store + ELEMS_DATA_OFFSET + 16),
        3.0_f64.to_bits()
    );
    let (grown, mem) = heap(
        "let a = []; let b = a; for (let i = 0; i < 5; i++) a.push(i); b",
        TYPE_ARRAY,
        0,
    );
    assert_eq!(u32_at(&mem, grown + ARRAY_LEN_OFFSET), 5, "grown len");
    let store = u32_at(&mem, grown + ARRAY_ELEMS_OFFSET);
    assert_eq!(u32_at(&mem, store + ELEMS_CAP_OFFSET), 8, "0 -> 4 -> 8");
    assert_eq!(
        u64_at(&mem, store + ELEMS_DATA_OFFSET + 32),
        4.0_f64.to_bits()
    );
    let (empty, mem) = heap("[]", TYPE_ARRAY, 0);
    assert_eq!(u32_at(&mem, empty + ARRAY_ELEMS_OFFSET), 0, "no store");

    let (promise, mem) = heap("Promise.resolve(7)", TYPE_PROMISE, 0);
    assert_eq!(
        u32_at(&mem, promise + PROMISE_STATE_OFFSET).cast_signed(),
        PROMISE_FULFILLED
    );
    assert_eq!(
        u64_at(&mem, promise + PROMISE_VALUE_OFFSET),
        7.0_f64.to_bits()
    );

    let (generator, mem) = heap(
        "function* gen() { yield 1 } let it = gen(); it.next(); it",
        TYPE_GENERATOR,
        0,
    );
    assert_eq!(
        u32_at(&mem, generator + GEN_STATE_OFFSET),
        1,
        "state after one yield"
    );
    assert_eq!(u32_at(&mem, generator + GEN_DONE_OFFSET), 0, "not done");
    let frame = u64_at(&mem, generator + GEN_ENV_OFFSET);
    assert_eq!((frame >> 48) & 0b111, 0b101, "frame is a heap block");

    // The payload is the TimeClip'd time value (truncated toward zero).
    let (date, mem) = heap("new Date(1234.5)", TYPE_DATE, 0);
    assert_eq!(u64_at(&mem, date + DATE_MS_OFFSET), 1234_f64.to_bits());
}

/// The number of `global.set` of the dirty-bits global in `bytes`.
fn dirty_bit_sets(bytes: &[u8]) -> usize {
    let mut sets = 0;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        if let wasmparser::Payload::CodeSectionEntry(body) = payload.expect("valid payload") {
            let mut ops = body.get_operators_reader().expect("operators");
            while !ops.eof() {
                if let wasmparser::Operator::GlobalSet { global_index } =
                    ops.read().expect("operator")
                    && global_index == super::DIRTY_GLOBAL
                {
                    sets += 1;
                }
            }
        }
    }
    sets
}

/// Compile `src` through the front end and codegen.
fn compile(src: &str) -> Vec<u8> {
    let program = crate::frontend::parse(src, crate::frontend::Dialect::Js).expect("parse");
    emit(&crate::codegen::lower(&program).expect("lower"))
}

#[test]
fn dirty_bits_are_set_only_when_tested() {
    // Intrinsics-sealed with inline namespaces: no guard, so `__set`,
    // `__define`, `__delete`, `__define_own`, `__set_integrity` and
    // `__set_proto` never touch the bits.
    assert_eq!(
        dirty_bit_sets(&compile("let a = []; a.push(Math.max(1, 2))")),
        0
    );
    // A push guard (not intrinsics-sealed) or a watched-namespace guard (an
    // escaping `Math`) makes the six helpers set the bits.
    assert_eq!(
        dirty_bit_sets(&compile("Object.prototype.x = 1; let a = []; a.push(1)")),
        6
    );
    assert_eq!(dirty_bit_sets(&compile("let m = Math; Math.max(1, 2)")), 6);
    // An unwatched namespace guard compares the member instead.
    assert_eq!(
        dirty_bit_sets(&compile("let o = Object; Object.keys({})")),
        0
    );
}

/// The byte FNV-1a the intern table used before strings hashed code units.
fn byte_fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5, |h: u32, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

#[test]
fn fnv1a_over_ascii_units_equals_the_byte_hash() {
    for text in [
        "",
        "a",
        "length",
        "prototype",
        "constructor",
        "0",
        "4294967294",
    ] {
        let units = crate::ir::JsStr::from(text);
        assert_eq!(
            super::fnv1a(units.as_units().iter().copied()),
            byte_fnv1a(text.as_bytes()),
            "{text:?}"
        );
    }
    // Pinned values of the 32-bit FNV-1a.
    assert_eq!(super::fnv1a([]), 0x811C_9DC5);
    assert_eq!(super::fnv1a("a".encode_utf16()), 0xE40C_292C);
    // A two-byte unit is one round, not two byte rounds.
    assert_eq!(
        super::fnv1a([0x0100]),
        (0x811C_9DC5u32 ^ 0x0100).wrapping_mul(0x0100_0193)
    );
}

#[test]
fn is_array_index_runs_on_code_units() {
    let index = |text: &str| super::is_array_index(&text.encode_utf16().collect::<Vec<_>>());
    assert!(index("0"));
    assert!(index("7"));
    assert!(index("4294967294"));
    assert!(!index("4294967295"));
    assert!(!index("01"));
    assert!(!index(""));
    assert!(!index("-1"));
    assert!(!index("1.5"));
    assert!(!index("42949672940"));
    // A full-width digit is not an index.
    assert!(!index("\u{FF11}"));
    assert!(!super::is_array_index(&[0x31, 0xD800]));
}

#[test]
fn static_string_blocks_are_canonical() {
    use crate::ir::JsStr;
    let header = |block: &[u8]| {
        (
            block[0],
            block[2],
            u32::from_le_bytes([block[4], block[5], block[6], block[7]]),
        )
    };
    let interned = abi::STR_FLAG_INTERNED;
    let two_byte = abi::STR_FLAG_TWO_BYTE;
    // `é` fits Latin-1: a one-byte block holding 0xE9, len 1.
    let block = super::sections::str_block(&JsStr::from("é"));
    assert_eq!(header(&block), (abi::TYPE_STRING, interned, 1));
    assert_eq!(&block[8..], [0xE9]);
    // `Ā` (U+0100) does not: a TWO_BYTE block, len 1, one UTF-16LE unit.
    let block = super::sections::str_block(&JsStr::from("Ā"));
    assert_eq!(header(&block), (abi::TYPE_STRING, interned | two_byte, 1));
    assert_eq!(&block[8..], [0x00, 0x01]);
    // A lone surrogate: TWO_BYTE, len 1.
    let block = super::sections::str_block(&[0xD800].into_iter().collect());
    assert_eq!(header(&block), (abi::TYPE_STRING, interned | two_byte, 1));
    assert_eq!(&block[8..], [0x00, 0xD8]);
    // An astral code point is two units.
    let block = super::sections::str_block(&JsStr::from("😀"));
    assert_eq!(header(&block), (abi::TYPE_STRING, interned | two_byte, 2));
    assert_eq!(&block[8..], [0x3D, 0xD8, 0x00, 0xDE]);
    // ASCII stays byte-identical; an index spelling keeps ARRAY_INDEX.
    let block = super::sections::str_block(&JsStr::from("12"));
    assert_eq!(
        header(&block),
        (abi::TYPE_STRING, interned | abi::STR_FLAG_ARRAY_INDEX, 2)
    );
    assert_eq!(&block[8..], *b"12");
}

#[test]
fn two_byte_literals_take_twice_the_data() {
    // Five units are 10 data bytes two-byte (a 24-byte block after the 8-byte
    // header and padding), but 5 one-byte.
    let program = expr_program(IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Str("ĀĀĀĀĀ".into())),
        rhs: Box::new(IrNode::Str("ééééé".into())),
    });
    let (offsets, _) = collect_strings(&program);
    let wide = *offsets.get("ĀĀĀĀĀ").expect("two-byte literal placed");
    let narrow = *offsets.get("ééééé").expect("one-byte literal placed");
    assert_eq!(narrow - wide, 24, "8 + 2 * 5 bytes, padded to 8");
}

#[test]
fn builtin_bodies_validate() {
    // Every builtin body is emitted into every module, so an unbalanced
    // builtin stack makes even the smallest program invalid wasm.
    validate(&compile("console.log(1)"));
    validate(&compile(
        "const s = 'Hello, World';
        const r = [
            s.indexOf('o'), s.includes('W'), s.slice(1, 3), s.toUpperCase(), s.toLowerCase(),
            s.toLocaleUpperCase(), s.toLocaleLowerCase(), s.charAt(1), s.trim(), s.trimStart(),
            s.trimEnd(), s.repeat(2), s.startsWith('H'), s.endsWith('d'), s.padStart(20, '*'),
            s.padEnd(20, '*'), s.substring(2, 5), s.split(', '), s.at(-1), s.charCodeAt(0),
            s.codePointAt(0), s.concat('!'), s.lastIndexOf('o'), s.localeCompare('a'),
            s.normalize(), s.search('o'), s.match('o'), s.matchAll('o'), s.replace('o', '0'),
            s.replaceAll('o', '0'), s.isWellFormed(), s.toWellFormed(),
        ];
        console.log(r.length)",
    ));
    validate(&compile(
        "const it = Iterator.from([1, 2, 3]).map((x) => x * 2).filter((x) => x > 2);
        console.log(it.take(1).toArray(), Math.sumPrecise([0.1, 0.2]));
        Promise.all([Promise.resolve(1), 2]).then((v) => console.log(v));
        Promise.any([Promise.reject(1)]).catch((e) => console.log(e.errors));",
    ));
}
