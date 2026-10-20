#[allow(clippy::wildcard_imports)]
use super::*;

/// Build the module's type and function sections and resolve the [`Helpers`].
///
/// Function 0 is `main` (`() -> i64`, type 0). Every user function shares the ONE
/// uniform-ABI signature `(a0..a_{K-1}, env, argc) -> i64` (type 1, `K = max_args`,
/// all `i64`), so a single `call_indirect` type serves them all; user function
/// `i` (0-based) is wasm function `i + 1`. The runtime helpers are appended LAST
/// so user-function indices `1..=n` stay stable. Because user functions now share
/// ONE deduplicated type, helper TYPE indices are distinct from their FUNCTION
/// indices, so the next free type index is tracked explicitly.
// One straight-line section builder: it grows with the helper roster (each helper
// adds a type + function registration), not with branching complexity.
#[allow(clippy::too_many_lines)]
pub(super) fn build_type_and_function_sections<'a>(
    program: &IrProgram,
    unwind: &'a RefCell<Unwind>,
    has_accessors: bool,
) -> (
    TypeSection,
    FunctionSection,
    Helpers<'a>,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
) {
    let mut types = TypeSection::new();
    let mut functions = FunctionSection::new();
    types.ty().function([], [ValType::I64]); // type 0: main
    functions.function(0);

    let n = program.functions.len();
    let has_funcs = n > 0;
    // The uniform user-function type (type 1) and the fixed positional arity K.
    // `func.params` counts the K argument slots plus the ABI_TRAILING_PARAMS
    // (env, argc) slots, so it is identical across every function;
    // `K = params - ABI_TRAILING_PARAMS`.
    let fn_type: u32 = 1;
    let max_args = program
        .functions
        .first()
        .map_or(0, |f| f.params.saturating_sub(ABI_TRAILING_PARAMS));
    if has_funcs {
        let leading = max_args + ABI_TRAILING_PARAMS; // K args + env + argc
        types
            .ty()
            .function(vec![ValType::I64; leading as usize], [ValType::I64]);
        for _ in 0..n {
            functions.function(fn_type);
        }
    }

    // Every DEFINED function index is shifted up by IMPORT_FUNC_COUNT because the
    // host imports occupy the lowest function indices: `main` is function
    // `IMPORT_FUNC_COUNT`, user function `i` is `i + 1 + IMPORT_FUNC_COUNT`, and
    // the helpers follow in the order `helper_types` lists them (and `emit`
    // appends their bodies).
    let mut next = IMPORT_FUNC_COUNT + 1 + u32::try_from(n).unwrap_or(u32::MAX);
    let mut index = || {
        next += 1;
        next - 1
    };
    let helpers = Helpers {
        unwind,
        // The host imports, in declaration order: `print` 0, `__convert` 1,
        // `__method` 2, `__promise` 3, `__then` 4, `__now` 5, `__parse_int` 6.
        print: 0,
        convert: 1,
        method: 2,
        promise: 3,
        then: 4,
        now: 5,
        parse_int: 6,
        alloc: index(),
        concat: index(),
        str_eq: index(),
        str_cmp: index(),
        str_index: index(),
        str_hash: index(),
        intern_slot: index(),
        intern_find: index(),
        intern: index(),
        index_value: index(),
        index_string: index(),
        to_key: index(),
        prop_find: index(),
        define: index(),
        get: index(),
        set: index(),
        has: index(),
        delete: index(),
        own_keys: index(),
        for_in_keys: index(),
        rest: index(),
        array_push: index(),
        array_concat: index(),
        json_escape: index(),
        json_str: index(),
        same_value_zero: index(),
        map_find: index(),
        map_set: index(),
        map_get: index(),
        map_has: index(),
        map_delete: index(),
        loose_eq: index(),
        str_to_array: index(),
        json_ws: index(),
        json_throw: index(),
        json_string: index(),
        json_number: index(),
        json_value: index(),
        json_array: index(),
        json_object: index(),
        json_parse: index(),
        gen_to_array: index(),
        nullish_throw: index(),
        prop_throw: index(),
        box_f64: index(),
        to_number: index(),
        // The `__invoke1` callback trampoline is appended after every helper
        // above; the `__async_step` trampoline follows it, then the two
        // `new Promise` settler functions.
        invoke1: index(),
        async_step: index(),
        // The settler funcref-TABLE slots sit right after the `n` user-function
        // slots (`0..n`), so slots `n` and `n + 1`. `0` when there are no functions
        // (no table — and `new Promise` cannot occur without an executor function).
        settle_resolve_slot: if has_funcs {
            u32::try_from(n).unwrap_or(u32::MAX)
        } else {
            0
        },
        settle_reject_slot: if has_funcs {
            u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1)
        } else {
            0
        },
        // The constructors follow the two settlers (present only with functions).
        new_string: index() + if has_funcs { 2 } else { 0 },
        fn_type: if has_funcs { fn_type } else { 0 },
        max_args,
        has_funcs,
        has_accessors,
    };
    // Helper types follow the main (type 0) and uniform (type 1, only when there
    // are functions) types, one per helper in function-index order.
    let mut next_type = if has_funcs { 2 } else { 1 };
    for (params, results) in helper_types() {
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        functions.function(next_type);
        next_type += 1;
    }
    // The two `new Promise` settler functions reuse the uniform function
    // type (type 1) — they are called through the funcref table like any function
    // value — so they add a FunctionSection entry but NO new type. Emitted only
    // when there are functions (the table exists and `new Promise` can occur).
    if has_funcs {
        functions.function(fn_type);
        functions.function(fn_type);
    }

    // Append the host import types LAST so they never disturb
    // the main/uniform/helper type indices above. All three are declared in the
    // IMPORT section (not the FunctionSection), so each adds a type but no defined
    // function: `print(bits: i64, flag: i32) -> ()`,
    // `__convert(bits: i64, op: i32) -> i64`, and
    // `__method(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64`.
    let print_type = next_type;
    types.ty().function([ValType::I64, ValType::I32], []);
    let convert_type = next_type + 1;
    types
        .ty()
        .function([ValType::I64, ValType::I32], [ValType::I64]);
    let method_type = next_type + 2;
    types.ty().function(
        [ValType::I64, ValType::I32, ValType::I64, ValType::I64],
        [ValType::I64],
    );
    // The host promise import types: `__promise(op: i32, a: i64, b: i64) ->
    // i64` and `__then(p: i64, on_f: i64, on_r: i64) -> i64`.
    let promise_type = next_type + 3;
    types
        .ty()
        .function([ValType::I32, ValType::I64, ValType::I64], [ValType::I64]);
    let then_type = next_type + 4;
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64], [ValType::I64]);
    // The host wall-clock import type (backing `Date`): `__now() -> f64`.
    let now_type = next_type + 5;
    types.ty().function([], [ValType::F64]);
    // The host `parseInt` import type: `__parse_int(s: i64, radix: i64) -> i64`.
    let parse_int_type = next_type + 6;
    types
        .ty()
        .function([ValType::I64, ValType::I64], [ValType::I64]);

    // The exported guest constructors, the LAST defined functions:
    // `__new_string(len: i32) -> i32`, `__new_array(len: i32) -> i32`, and
    // `__new_promise() -> i32`. Their types follow the import types.
    types.ty().function([ValType::I32], [ValType::I32]);
    functions.function(next_type + 7);
    functions.function(next_type + 7);
    types.ty().function([], [ValType::I32]);
    functions.function(next_type + 8);

    (
        types,
        functions,
        helpers,
        print_type,
        convert_type,
        method_type,
        promise_type,
        then_type,
        now_type,
        parse_int_type,
    )
}

/// The `(params, results)` of every runtime helper, in function-index order
/// (the order [`build_type_and_function_sections`] assigns their indices and
/// [`emit`] appends their bodies).
fn helper_types() -> Vec<(&'static [ValType], &'static [ValType])> {
    use ValType::{F64, I32, I64};
    vec![
        // `__alloc(size) -> base`.
        (&[I32], &[I32]),
        // `__concat` / `__str_eq` / `__str_cmp` / `__str_index`: `(i32, i32) -> i32`.
        (&[I32, I32], &[I32]),
        (&[I32, I32], &[I32]),
        (&[I32, I32], &[I32]),
        (&[I32, I32], &[I32]),
        // Interning: `__str_hash(s)`, `__intern_slot(s)`, `__intern_find(s)`,
        // `__intern(s)`, `__index_value(s)`, `__index_string(n)`: `(i32) -> i32`.
        (&[I32], &[I32]),
        (&[I32], &[I32]),
        (&[I32], &[I32]),
        (&[I32], &[I32]),
        (&[I32], &[I32]),
        (&[I32], &[I32]),
        // `__to_key(v, insert) -> key`.
        (&[I64, I32], &[I32]),
        // `__prop_find(o, key) -> entry`.
        (&[I32, I32], &[I32]),
        // `__define(o, key, v, attrs)`.
        (&[I32, I32, I64, I32], &[]),
        // `__get(recv, key) -> v`.
        (&[I64, I32], &[I64]),
        // `__set(recv, key, v)`.
        (&[I64, I32, I64], &[]),
        // `__has(key, obj) -> i32`.
        (&[I32, I64], &[I32]),
        // `__delete(obj, key)`.
        (&[I64, I32], &[]),
        // `__own_keys(obj, mode) -> array`.
        (&[I64, I32], &[I64]),
        // `__for_in_keys(obj) -> array`.
        (&[I64], &[I64]),
        // `__rest(src, excl) -> object`.
        (&[I64, I64], &[I64]),
        // `__array_push(base, value) -> new_base`.
        (&[I32, I64], &[I32]),
        // `__array_concat(dst, src) -> new_base`.
        (&[I32, I32], &[I32]),
        // `__json_escape(str) -> str` and `__json_str(v) -> str`.
        (&[I32], &[I32]),
        (&[I64], &[I32]),
        // The Map/Set helpers: `__same_value_zero(a, b) -> i32`,
        // `__map_find(handle, key) -> i32`, `__map_set(handle, key, val)`,
        // `__map_get(handle, key) -> i64`, `__map_has(handle, key) -> i32`,
        // `__map_delete(handle, key) -> i32`.
        (&[I64, I64], &[I32]),
        (&[I32, I64], &[I32]),
        (&[I32, I64, I64], &[]),
        (&[I32, I64], &[I64]),
        (&[I32, I64], &[I32]),
        (&[I32, I64], &[I32]),
        // `__loose_eq(a, b) -> i32` and `__str_to_array(s) -> i64`.
        (&[I64, I64], &[I32]),
        (&[I64], &[I64]),
        // The `JSON.parse` family: `__json_ws()`, `__json_throw() -> i64`,
        // `__json_string`/`__json_number`/`__json_value`/`__json_array`/
        // `__json_object() -> i64`, `__json_parse(s) -> i64`.
        (&[], &[]),
        (&[], &[I64]),
        (&[], &[I64]),
        (&[], &[I64]),
        (&[], &[I64]),
        (&[], &[I64]),
        (&[], &[I64]),
        (&[I64], &[I64]),
        // `__gen_to_array(gen) -> i64`.
        (&[I64], &[I64]),
        // `__nullish_throw(recv, key, set)` and `__prop_throw(recv, key, mode)`.
        (&[I64, I64, I32], &[]),
        (&[I64, I64, I32], &[]),
        // `__box_f64(x) -> i64` and `__to_number(v) -> i64`.
        (&[F64], &[I64]),
        (&[I64], &[I64]),
        // The `__invoke1(fn, arg)` and `__async_step(gen, sent)` trampolines.
        (&[I64, I64], &[I64]),
        (&[I64, I64], &[I64]),
    ]
}

/// Build the module's global section: the mutable bump-allocator heap pointer
/// (`$__heap_ptr`, index [`HEAP_PTR_GLOBAL`], starting at `heap_ptr_init` — the
/// first free offset after all static string-literal data so the allocator never
/// overwrites them), followed by the exception-unwinding globals — the
/// pending flag (`$exc_pending`, i32) and the in-flight value (`$exc_value`, i64),
/// both zero-initialized. A `throw` sets the latter two; every throwing call site
/// checks the flag to unwind. Two further zero-initialized globals — the
/// pending-completion kind (`$cf_kind`, i32) and routed return value
/// (`$cf_value`, i64) — carry a `return`/`break`/`continue` through the `finally`
/// clauses it must run.
pub(super) fn build_globals(
    heap_ptr_init: i32,
    intern: &InternTable,
    module_cells: u32,
) -> GlobalSection {
    let mut globals = GlobalSection::new();
    let mut_i32 = |init| {
        (
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            ConstExpr::i32_const(init),
        )
    };
    let mut_i64 = || {
        (
            GlobalType {
                val_type: ValType::I64,
                mutable: true,
                shared: false,
            },
            ConstExpr::i64_const(0),
        )
    };
    let (heap_ty, heap_init) = mut_i32(heap_ptr_init);
    globals.global(heap_ty, &heap_init);
    let (pending_ty, pending_init) = mut_i32(0);
    globals.global(pending_ty, &pending_init);
    let (exc_val_ty, exc_val_init) = mut_i64();
    globals.global(exc_val_ty, &exc_val_init);
    // The pending-completion globals: the kind ($cf_kind, i32) and the
    // routed return value ($cf_value, i64), driving a return/break/continue
    // through the `finally` clauses it must run. Internal, not exported.
    let (cf_kind_ty, cf_kind_init) = mut_i32(0);
    globals.global(cf_kind_ty, &cf_kind_init);
    let (cf_val_ty, cf_val_init) = mut_i64();
    globals.global(cf_val_ty, &cf_val_init);
    // The `this`-binding carrier ($__this, i64, index THIS_GLOBAL). Init to
    // the `undefined` bits (NOT zero) so `this` reads `undefined` before any method
    // call sets it. Internal, not exported.
    globals.global(
        GlobalType {
            val_type: ValType::I64,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i64_const(UNDEFINED_BITS),
    );
    // The `JSON.parse` cursor globals (data offset, position, length; all i32),
    // zero-initialized and set at each `__json_parse` entry.
    for _ in 0..3 {
        let (ty, init) = mut_i32(0);
        globals.global(ty, &init);
    }
    // The intern-table globals (base, cap, count; all i32), initialized to the
    // compile-time table in the data segment.
    for init in [intern.base, intern.cap, intern.count] {
        let (ty, init) = mut_i32(init);
        globals.global(ty, &init);
    }
    // The module-cell pointers (i64, index `MODULE_CELL_GLOBAL_BASE + n`), set in
    // `main`'s prologue before any user code runs.
    for _ in 0..module_cells {
        let (ty, init) = mut_i64();
        globals.global(ty, &init);
    }
    globals
}

/// Build the data section from the string-literal offset map: one active segment
/// per literal, each a string block `[type][gc][flags][class][len:u32 LE][utf8]`
/// written at its compile-time offset (its `flags` already `INTERNED`, plus
/// `ARRAY_INDEX` for a canonical index spelling), in ascending offset order for
/// deterministic output, then the intern table's u32 slots.
pub(super) fn emit_data_section(
    offsets: &HashMap<String, i32>,
    intern: &InternTable,
) -> DataSection {
    let mut segments: Vec<(&String, i32)> = offsets.iter().map(|(s, o)| (s, *o)).collect();
    segments.sort_by_key(|(_, offset)| *offset);
    let mut data = DataSection::new();
    let header_size = HEADER_SIZE as usize;
    for (text, offset) in segments {
        let mut blob = Vec::with_capacity(header_size + 4 + text.len());
        // Block header: the TYPE_STRING type byte at offset 0, a zero `gc` byte,
        // the string `flags`, and a zero `class` byte.
        let index_flag = if is_array_index(text) {
            abi::STR_FLAG_ARRAY_INDEX
        } else {
            0
        };
        blob.push(u8::try_from(TYPE_STRING).unwrap_or(0));
        blob.push(0);
        blob.push(abi::STR_FLAG_INTERNED | index_flag);
        blob.resize(header_size, 0);
        let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        blob.extend_from_slice(&len.to_le_bytes());
        blob.extend_from_slice(text.as_bytes());
        data.active(0, &ConstExpr::i32_const(offset), blob);
    }
    let table: Vec<u8> = intern
        .slots
        .iter()
        .flat_map(|slot| slot.to_le_bytes())
        .collect();
    data.active(0, &ConstExpr::i32_const(intern.base), table);
    data
}

/// Collect every distinct string literal in `program` and assign each a
/// compile-time byte offset in linear memory.
///
/// Literals are placed from [`HEAP_BASE`] in first-appearance order, each
/// occupying its `[type][pad×3][len:u32 LE][utf8]` block padded up to
/// [`BLOCK_ALIGN`] so every blob base is 8-aligned. Identical literals are
/// deduplicated onto one offset. Returns the offset map plus the first free
/// offset after all literal data (8-aligned), where the intern table is placed.
pub(super) fn collect_strings(program: &IrProgram) -> (HashMap<String, i32>, i32) {
    // `"length"` is always interned: the property helpers compare a key against
    // it to resolve a string's or array's exotic `length`.
    let mut order: Vec<&str> = vec![INDEX_LENGTH_KEY];
    let mut seen: HashSet<&str> = HashSet::from([INDEX_LENGTH_KEY]);
    for stmt in &program.stmts {
        collect_stmt(stmt, &mut order, &mut seen);
    }
    for func in &program.functions {
        for stmt in &func.body {
            collect_stmt(stmt, &mut order, &mut seen);
        }
        // A generator state machine carries its code in segments, not `body`:
        // walk each segment's statements and its terminator value.
        if let Some(segments) = &func.machine {
            for segment in segments {
                for stmt in &segment.stmts {
                    collect_stmt(stmt, &mut order, &mut seen);
                }
                match &segment.term {
                    crate::ir::GenTerm::Yield { value, .. }
                    | crate::ir::GenTerm::Branch { cond: value, .. }
                    | crate::ir::GenTerm::Return(Some(value)) => {
                        collect_node(value, &mut order, &mut seen);
                    }
                    crate::ir::GenTerm::Return(None) | crate::ir::GenTerm::Jump(_) => {}
                }
            }
        }
    }

    let mut offsets = HashMap::with_capacity(order.len());
    let mut next = HEAP_BASE;
    for text in order {
        offsets.insert(text.to_owned(), next);
        // Each blob is `[type=0][pad×3][len:u32][utf8]` (block header): the block
        // header, the 4-byte length header, plus the byte length, padded to 8.
        let blob_len = HEADER_SIZE + 4 + i32::try_from(text.len()).unwrap_or(i32::MAX);
        next = next
            .saturating_add(blob_len)
            .saturating_add(BLOCK_ALIGN - 1)
            & -BLOCK_ALIGN;
    }
    (offsets, next)
}

/// Walk one statement, recording each distinct string literal in appearance
/// order (see [`collect_strings`]).
pub(super) fn collect_stmt<'a>(
    stmt: &'a IrStmt,
    order: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) {
    match stmt {
        IrStmt::Decl { init, .. } => collect_node(init, order, seen),
        IrStmt::Expr(node) => collect_node(node, order, seen),
        IrStmt::Return(value) => {
            if let Some(node) = value {
                collect_node(node, order, seen);
            }
        }
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            collect_node(cond, order, seen);
            for stmt in then_body.iter().chain(else_body) {
                collect_stmt(stmt, order, seen);
            }
        }
        IrStmt::While {
            cond, body, update, ..
        } => {
            collect_node(cond, order, seen);
            for stmt in body.iter().chain(update) {
                collect_stmt(stmt, order, seen);
            }
        }
        IrStmt::Switch { disc, cases, .. } => {
            collect_node(disc, order, seen);
            for case in cases {
                if let Some(test) = &case.test {
                    collect_node(test, order, seen);
                }
                for stmt in &case.body {
                    collect_stmt(stmt, order, seen);
                }
            }
        }
        IrStmt::Break(_) | IrStmt::Continue(_) => {}
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            let clauses = catch_body.iter().chain(finally_body).flatten();
            for stmt in body.iter().chain(clauses) {
                collect_stmt(stmt, order, seen);
            }
        }
    }
}

/// Walk one expression node, recording each distinct string literal in
/// appearance order (see [`collect_strings`]).
#[allow(clippy::too_many_lines)]
pub(super) fn collect_node<'a>(
    node: &'a IrNode,
    order: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) {
    match node {
        IrNode::Str(text) => {
            if seen.insert(text.as_str()) {
                order.push(text.as_str());
            }
        }
        IrNode::Binary { lhs, rhs, .. } | IrNode::Logical { lhs, rhs, .. } => {
            collect_node(lhs, order, seen);
            collect_node(rhs, order, seen);
        }
        // The nullish guard interns its error strings and the read key.
        IrNode::NullishCheck { value, key } => {
            seed_nullish_strs(order, seen);
            if seen.insert(key.as_str()) {
                order.push(key.as_str());
            }
            collect_node(value, order, seen);
        }
        // `ArrayPush` carries only its pushed value plus the receiver guard's
        // strings (its local read/rebind has no string literals).
        IrNode::ArrayPush { value, .. } => {
            seed_nullish_strs(order, seen);
            if seen.insert("push") {
                order.push("push");
            }
            collect_node(value, order, seen);
        }
        IrNode::Assign { value, .. } => {
            collect_node(value, order, seen);
        }
        // A direct call and a `console.log` both just collect their argument
        // literals (`console.log` has no callee expression to walk).
        IrNode::Call { args, .. } | IrNode::ConsoleLog { args } | IrNode::MathCall { args, .. } => {
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        // An indirect call and a `super(..)`/`super.m(..)` `ThisCall` both intern
        // their callee expression and every argument, plus the not-a-function
        // TypeError strings (a function-less program's indirect call throws it).
        IrNode::CallIndirect { callee, args } | IrNode::ThisCall { callee, args, .. } => {
            seed_type_error_strs(order, seen);
            collect_node(callee, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        // `typeof` interns the six type-string words so the strict-equality path
        // can compare a `typeof` result (a boxed immediate) against a string
        // literal by content — see `emit_typeof_to_string`.
        IrNode::TypeOf(operand) => {
            for word in crate::abi::TYPEOF_STRINGS {
                if seen.insert(word) {
                    order.push(word);
                }
            }
            collect_node(operand, order, seen);
        }
        IrNode::StrLen(operand)
        | IrNode::Not(operand)
        | IrNode::IsHole(operand)
        | IrNode::CellNew(operand)
        | IrNode::CellGet(operand)
        | IrNode::Throw(operand)
        // A generator `yield e` / `IsGenerator` carries only its operand.
        | IrNode::Yield(operand)
        | IrNode::IsGenerator(operand)
        | IrNode::ObjectBuiltin { arg: operand, .. } => {
            collect_node(operand, order, seen);
        }
        IrNode::ConvertCall { arg, radix, .. } => {
            collect_node(arg, order, seen);
            if let Some(radix) = radix {
                collect_node(radix, order, seen);
            }
        }
        IrNode::FuncValue { captures, .. } => {
            for capture in captures {
                collect_node(capture, order, seen);
            }
        }
        IrNode::CellSet { cell, value } => {
            collect_node(cell, order, seen);
            collect_node(value, order, seen);
        }
        IrNode::Array(elems) => {
            for elem in elems {
                collect_node(elem, order, seen);
            }
        }
        IrNode::ArraySpread(parts) => {
            for part in parts {
                collect_spread_elem(part, order, seen);
            }
        }
        IrNode::CallSpread { target, args } => {
            if let SpreadCallee::Indirect(callee) = target {
                if !is_static_function(callee) {
                    seed_type_error_strs(order, seen);
                }
                collect_node(callee, order, seen);
            }
            collect_node(args, order, seen);
        }
        IrNode::MathSpread { args, .. } => {
            collect_node(args, order, seen);
        }
        IrNode::Object { entries, proto, .. } => {
            // Each static key is an interned string (its blob must live in the
            // data segment so a literal can name it); recurse into the proto,
            // each computed key, and each value.
            if let Some(proto) = proto {
                collect_node(proto, order, seen);
            }
            for (key, value) in entries {
                match key {
                    PropKey::Static(key) | PropKey::Hidden(key) => {
                        if seen.insert(key.as_str()) {
                            order.push(key.as_str());
                        }
                    }
                    PropKey::Computed(key) => collect_node(key, order, seen),
                }
                collect_node(value, order, seen);
            }
        }
        IrNode::ArrayHof {
            array,
            callback,
            init,
            ..
        } => {
            if !is_static_function(callback) {
                seed_type_error_strs(order, seen);
            }
            collect_node(array, order, seen);
            collect_node(callback, order, seen);
            if let Some(init) = init {
                collect_node(init, order, seen);
            }
        }
        IrNode::MethodCall { recv, args, .. } => {
            collect_node(recv, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        IrNode::MethodCallUser { obj, key, args, .. } => {
            // Seed the runtime callee-is-a-function guard's branded TypeError
            // strings so their data-segment blobs exist when the guard throws.
            seed_type_error_strs(order, seen);
            seed_nullish_strs(order, seen);
            collect_node(obj, order, seen);
            collect_node(key, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        IrNode::New {
            instance,
            ctor,
            args,
            ..
        } => {
            seed_type_error_strs(order, seen);
            collect_node(instance, order, seen);
            collect_node(ctor, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        IrNode::ObjectRest { source, exclude } => {
            collect_node(source, order, seen);
            collect_node(exclude, order, seen);
        }
        IrNode::Index { obj, index } => {
            seed_nullish_strs(order, seen);
            collect_node(obj, order, seen);
            collect_node(index, order, seen);
        }
        // An instance accessor: recurse into the getter/setter function values
        // and intern the sentinel that GATES the member read/write invocation paths
        // (present in the string table iff the program builds any accessor box).
        IrNode::MakeAccessor { getter, setter } => {
            if seen.insert(crate::abi::ACCESSOR_MARKER_KEY) {
                order.push(crate::abi::ACCESSOR_MARKER_KEY);
            }
            collect_node(getter, order, seen);
            collect_node(setter, order, seen);
        }
        IrNode::ObjectHas { key, obj } => {
            collect_node(key, order, seen);
            collect_node(obj, order, seen);
        }
        IrNode::InstanceOf { value, kind } => {
            collect_node(value, order, seen);
            match kind {
                // A user-class test reads the class prototype; the `@@prototype`
                // key (and the class read's literals) are interned by recursing
                // into the stored `<class>["@@prototype"]` expr.
                InstanceOfKind::Class(class_proto) => collect_node(class_proto, order, seen),
                // An error test reads the hidden `@@errorkind` marker via
                // `__get`, so its key must be interned even when the program
                // never constructs an error literally.
                InstanceOfKind::Error | InstanceOfKind::TypeError | InstanceOfKind::RangeError => {
                    if seen.insert(crate::abi::ERROR_KIND_KEY) {
                        order.push(crate::abi::ERROR_KIND_KEY);
                    }
                }
                // The runtime-TYPE-BYTE builtins read the receiver's own type byte
                // only — no interned key or literal to seed.
                InstanceOfKind::Array
                | InstanceOfKind::Function
                | InstanceOfKind::Date
                | InstanceOfKind::Object => {}
            }
        }
        IrNode::ObjectDelete { obj, key } => {
            seed_prop_strs(order, seen);
            collect_node(obj, order, seen);
            collect_node(key, order, seen);
        }
        IrNode::JsonStringify { arg } => {
            collect_node(arg, order, seen);
            // Seed the serializer's literal token strings (`null`/`true`/`false`
            // and the structural punctuation) so their `[len][utf8]` blocks exist
            // in the data segment for `__json_str` to splice via `__concat`.
            for tok in JSON_TOKEN_STRS {
                if seen.insert(tok) {
                    order.push(tok);
                }
            }
        }
        IrNode::JsonParse { arg } => {
            collect_node(arg, order, seen);
            // Seed the syntax-error object's static strings so `__json_throw` can
            // build a branded `Error` on malformed input.
            for s in JSON_PARSE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
        }
        IrNode::IndexSet { obj, index, value } => {
            seed_nullish_strs(order, seen);
            seed_prop_strs(order, seen);
            collect_node(obj, order, seen);
            collect_node(index, order, seen);
            collect_node(value, order, seen);
        }
        IrNode::IndexOpSet {
            obj, index, rhs, ..
        } => {
            seed_nullish_strs(order, seen);
            seed_prop_strs(order, seen);
            collect_node(obj, order, seen);
            collect_node(index, order, seen);
            collect_node(rhs, order, seen);
        }
        IrNode::Ternary {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_node(cond, order, seen);
            collect_node(then_branch, order, seen);
            collect_node(else_branch, order, seen);
        }
        // A Map/Set method threads its method name (used as the user-method-fallback
        // key) into the string table, then recurses into the receiver and arguments.
        IrNode::MapMethod {
            recv,
            args,
            method_name,
            ..
        } => {
            if seen.insert(method_name.as_str()) {
                order.push(method_name.as_str());
            }
            // The unknown-collection-method fallback lowers to a user method call,
            // which the runtime callee guard protects — seed its TypeError strings too.
            seed_type_error_strs(order, seen);
            collect_node(recv, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        // A promise / generator method threads its method name (the user-method
        // fallback key) into the string table; the generator path builds a
        // `{ value, done }` result, and the guarded fallback throws a nullish /
        // not-a-function TypeError — seed all of those strings too.
        IrNode::ReceiverMethod { op, recv, args, .. } => {
            let keys: &[&'static str] = if matches!(op, ReceiverOp::Next) {
                &["value", "done"]
            } else {
                &[]
            };
            let name = [op.name()];
            for &s in name.iter().chain(keys).chain(&INTERNAL_TYPE_ERROR_STRS) {
                if seen.insert(s) {
                    order.push(s);
                }
            }
            seed_nullish_strs(order, seen);
            collect_node(recv, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        IrNode::DateMethod {
            op,
            recv,
            args,
            method_name,
            ..
        } => {
            if seen.insert(method_name.as_str()) {
                order.push(method_name.as_str());
            }
            // The non-Date fallback lowers to a guarded user method call — seed
            // its TypeError strings too.
            seed_type_error_strs(order, seen);
            // `toISOString()` returns the interned `"Invalid Date"` literal for a
            // non-finite timestamp, so its `[len][utf8]` blob must exist.
            if matches!(op, DateOp::ToIsoString) && seen.insert(INVALID_DATE) {
                order.push(INVALID_DATE);
            }
            collect_node(recv, order, seen);
            for arg in args {
                collect_node(arg, order, seen);
            }
        }
        // The promise primitives carry only sub-expressions to walk.
        IrNode::PromiseResolve(operand)
        | IrNode::PromiseReject(operand)
        | IrNode::Await(operand) => collect_node(operand, order, seen),
        IrNode::PromiseThen {
            promise,
            on_fulfilled,
            on_rejected,
        } => {
            collect_node(promise, order, seen);
            collect_node(on_fulfilled, order, seen);
            collect_node(on_rejected, order, seen);
        }
        IrNode::PromiseFinally {
            promise,
            on_finally,
        } => {
            collect_node(promise, order, seen);
            collect_node(on_finally, order, seen);
        }
        // `new Promise(executor)` carries only the executor sub-expression to walk
        // (the settler functions are runtime-emitted, no literals).
        IrNode::PromiseCtor { executor, .. } => {
            if !is_static_function(executor) {
                seed_type_error_strs(order, seen);
            }
            collect_node(executor, order, seen);
        }
        // `AsyncSpawn` wraps the inner `MakeGenerator` — walk it so the
        // constructor's per-parameter argument reads are interned.
        IrNode::AsyncSpawn(make_generator) => collect_node(make_generator, order, seen),
        // `MakeGenerator` carries the constructor's per-parameter argument reads.
        IrNode::MakeGenerator { slots, .. } => {
            for slot in slots {
                collect_node(slot, order, seen);
            }
        }
        // `gen.next(sent)` builds a `{ value, done }` result object, so both keys'
        // `[len][utf8]` blobs must live in the data segment; a non-generator
        // receiver throws a branded `TypeError`, so seed its strings too.
        IrNode::GenResume { handle, sent } => {
            for key in ["value", "done"] {
                if seen.insert(key) {
                    order.push(key);
                }
            }
            seed_type_error_strs(order, seen);
            collect_node(handle, order, seen);
            collect_node(sent, order, seen);
        }
        // The `Date` builtins carry no string literals; recurse into
        // the timestamp / receiver sub-expressions (`Date.now()` has neither).
        IrNode::DateNew { ms } => {
            if let Some(ms) = ms {
                collect_node(ms, order, seen);
            }
        }
        IrNode::DateGetTime { recv } => collect_node(recv, order, seen),
        // `pop` only needs its receiver guard's strings.
        IrNode::ArrayPop { .. } => {
            seed_nullish_strs(order, seen);
            if seen.insert("pop") {
                order.push("pop");
            }
        }
        IrNode::Num(_)
        | IrNode::Bool(_)
        | IrNode::Null
        | IrNode::Undefined
        | IrNode::Local(_)
        | IrNode::EnvLoad(_)
        | IrNode::ModuleCell(_)
        | IrNode::SentValue
        | IrNode::ExcValue
        | IrNode::ThisGlobal
        // `new Map()` / `new Set()` and `Date.now()` carry no string literals.
        | IrNode::MapNew { .. }
        | IrNode::DateNow
        | IrNode::IncDec { .. } => {}
    }
}

/// Seed the nullish-receiver guard's [`NULLISH_ERROR_STRS`] so `__nullish_throw`
/// finds their `[len][utf8]` blobs in the data segment; see [`collect_node`].
fn seed_nullish_strs(order: &mut Vec<&str>, seen: &mut HashSet<&str>) {
    for s in NULLISH_ERROR_STRS {
        if seen.insert(s) {
            order.push(s);
        }
    }
}

/// Seed the not-a-function guard's [`INTERNAL_TYPE_ERROR_STRS`] so the branded
/// `TypeError` it throws finds their `[len][utf8]` blobs in the data segment;
/// see [`collect_node`].
fn seed_type_error_strs(order: &mut Vec<&str>, seen: &mut HashSet<&str>) {
    for s in INTERNAL_TYPE_ERROR_STRS {
        if seen.insert(s) {
            order.push(s);
        }
    }
}

/// Seed the member-store guard's [`PROP_ERROR_STRS`] so `__prop_throw` finds
/// their `[len][utf8]` blobs in the data segment; see [`collect_node`].
fn seed_prop_strs(order: &mut Vec<&str>, seen: &mut HashSet<&str>) {
    let keyed = PROP_KEYED_MSGS
        .iter()
        .flat_map(|&(_, prefix, suffix)| [prefix, suffix]);
    for s in PROP_ERROR_STRS.into_iter().chain(keyed) {
        if seen.insert(s) {
            order.push(s);
        }
    }
}

/// Record the string literals of one spread element (its plain-item or
/// spread-source sub-expression); see [`collect_node`].
pub(super) fn collect_spread_elem<'a>(
    part: &'a SpreadElem,
    order: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) {
    match part {
        SpreadElem::Item(node) | SpreadElem::Spread(node) => collect_node(node, order, seen),
    }
}
