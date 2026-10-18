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
    // host `print` import occupies the lowest function indices (S14.1): `main` is
    // function `IMPORT_FUNC_COUNT`, user function `i` is `i + 1 + IMPORT_FUNC_COUNT`,
    // and the helpers follow.
    let alloc_index = IMPORT_FUNC_COUNT + 1 + u32::try_from(n).unwrap_or(u32::MAX);
    let concat_index = alloc_index + 1;
    let str_eq_index = concat_index + 1;
    let str_cmp_index = str_eq_index + 1;
    let str_index_index = str_cmp_index + 1;
    let obj_get_index = str_index_index + 1;
    let helpers = Helpers {
        unwind,
        // The host `print` import is function index 0, `__convert` index 1, and
        // `__method` index 2 (imports come first, in declaration order).
        print: 0,
        convert: 1,
        method: 2,
        // The host promise imports (P4.1) follow the S14 imports in declaration
        // order: `__promise` index 3, `__then` index 4. The host wall clock
        // (`__now`, P-series `Date`) is appended last, index 5.
        promise: 3,
        then: 4,
        now: 5,
        alloc: alloc_index,
        concat: concat_index,
        str_eq: str_eq_index,
        str_cmp: str_cmp_index,
        str_index: str_index_index,
        obj_get: obj_get_index,
        obj_set: obj_get_index + 1,
        array_push: obj_get_index + 2,
        obj_rest: obj_get_index + 3,
        array_concat: obj_get_index + 4,
        obj_keys: obj_get_index + 5,
        obj_values: obj_get_index + 6,
        obj_entries: obj_get_index + 7,
        obj_has: obj_get_index + 8,
        obj_delete: obj_get_index + 9,
        json_escape: obj_get_index + 10,
        json_str: obj_get_index + 11,
        js_eq: obj_get_index + 12,
        map_find: obj_get_index + 13,
        map_set: obj_get_index + 14,
        map_get: obj_get_index + 15,
        map_has: obj_get_index + 16,
        map_delete: obj_get_index + 17,
        // `__loose_eq` (loose `==`) and `__str_to_array` (string spread) follow the
        // Map/Set helpers, before the `JSON.parse` family.
        loose_eq: obj_get_index + 18,
        str_to_array: obj_get_index + 19,
        // The in-wasm `JSON.parse` helpers, in emitted order.
        json_ws: obj_get_index + 20,
        json_throw: obj_get_index + 21,
        json_string: obj_get_index + 22,
        json_number: obj_get_index + 23,
        json_value: obj_get_index + 24,
        json_array: obj_get_index + 25,
        json_object: obj_get_index + 26,
        array_to_object: obj_get_index + 27,
        json_parse: obj_get_index + 28,
        // `__gen_to_array` (P4.2 M3) is appended after the Map/Set helpers and
        // before `__invoke1`.
        gen_to_array: obj_get_index + 29,
        // `__nullish_throw` (the nullish-receiver guard's error build) follows it.
        nullish_throw: obj_get_index + 30,
        // The `__invoke1` callback trampoline (P4.1) is appended after every helper
        // above; the `__async_step` trampoline (P4.3 M4) follows it, then the two
        // `new Promise` settler functions.
        invoke1: obj_get_index + 31,
        async_step: obj_get_index + 32,
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
        fn_type: if has_funcs { fn_type } else { 0 },
        max_args,
        has_funcs,
        has_accessors,
    };
    // Helper types follow the main (type 0) and uniform (type 1, only when there
    // are functions) types, tracked by the next free type index. Scoped in a
    // block so the closure's borrow of `types`/`functions` ends before they are
    // returned.
    // The first free type index after main (type 0) and the uniform function type
    // (type 1, only when there are functions).
    let helper_type_base = if has_funcs { 2 } else { 1 };
    {
        let mut next_type = helper_type_base;
        let mut helper = |params: &[ValType], results: &[ValType]| {
            types
                .ty()
                .function(params.iter().copied(), results.iter().copied());
            functions.function(next_type);
            next_type += 1;
        };
        // `__alloc` (`(i32) -> i32`), then `__concat`/`__str_eq`/`__str_cmp`/
        // `__str_index` (each `(i32, i32) -> i32`), in function-index order.
        helper(&[ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        // `__obj_get(obj_ptr: i32, key_ptr: i32) -> i64` and
        // `__obj_set(obj_ptr: i32, key_ptr: i32, val: i64)`.
        helper(&[ValType::I32, ValType::I32], &[ValType::I64]);
        helper(&[ValType::I32, ValType::I32, ValType::I64], &[]);
        // `__array_push(base: i32, value: i64) -> new_base: i32` (S14.4).
        helper(&[ValType::I32, ValType::I64], &[ValType::I32]);
        // `__obj_rest(obj_ptr: i32, excl_ptr: i32) -> new_base: i32` (P1.2).
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        // `__array_concat(dst: i32, src: i32) -> new_base: i32` (P1.5).
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        // `__obj_keys`/`__obj_values`/`__obj_entries(obj_ptr: i32) -> array_base:
        // i32` are appended LAST (P3.2), each `(i32) -> i32`.
        helper(&[ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32], &[ValType::I32]);
        // `__obj_has(obj_ptr: i32, key_ptr: i32) -> i32` and
        // `__obj_delete(obj_ptr: i32, key_ptr: i32) -> ()` (P3.3).
        helper(&[ValType::I32, ValType::I32], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I32], &[]);
        // `__json_escape(str_ptr: i32) -> str_base: i32` and the recursive
        // `__json_str(v: i64) -> str_base: i32` serializer (P3.4).
        helper(&[ValType::I32], &[ValType::I32]);
        helper(&[ValType::I64], &[ValType::I32]);
        // The Map/Set runtime helpers (P3.5): `__js_eq(a: i64, b: i64) -> i32`,
        // `__map_find(handle: i32, key: i64) -> i32`,
        // `__map_set(handle: i32, key: i64, val: i64)`,
        // `__map_get(handle: i32, key: i64) -> i64`,
        // `__map_has(handle: i32, key: i64) -> i32`, and
        // `__map_delete(handle: i32, key: i64) -> i32`.
        helper(&[ValType::I64, ValType::I64], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I64], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I64, ValType::I64], &[]);
        helper(&[ValType::I32, ValType::I64], &[ValType::I64]);
        helper(&[ValType::I32, ValType::I64], &[ValType::I32]);
        helper(&[ValType::I32, ValType::I64], &[ValType::I32]);
        // `__loose_eq(a: i64, b: i64) -> i32` (loose `==`) and
        // `__str_to_array(s: i64) -> i64` (string spread).
        helper(&[ValType::I64, ValType::I64], &[ValType::I32]);
        helper(&[ValType::I64], &[ValType::I64]);
        // The `JSON.parse` family, in emitted order: `__json_ws() -> ()`,
        // `__json_throw() -> i64`, `__json_string() -> i64`,
        // `__json_number() -> i64`, `__json_value() -> i64`,
        // `__json_array() -> i64`, `__json_object() -> i64`,
        // `__array_to_object(pairs: i32) -> i32`, `__json_parse(s: i64) -> i64`.
        helper(&[], &[]);
        helper(&[], &[ValType::I64]);
        helper(&[], &[ValType::I64]);
        helper(&[], &[ValType::I64]);
        helper(&[], &[ValType::I64]);
        helper(&[], &[ValType::I64]);
        helper(&[], &[ValType::I64]);
        helper(&[ValType::I32], &[ValType::I32]);
        helper(&[ValType::I64], &[ValType::I64]);
        // `__gen_to_array(gen: i64) -> i64` (P4.2 M3), before the trampoline.
        helper(&[ValType::I64], &[ValType::I64]);
        // `__nullish_throw(recv: i64, key: i64, set: i32) -> ()`.
        helper(&[ValType::I64, ValType::I64, ValType::I32], &[]);
        // The `__invoke1(fn: i64, arg: i64) -> i64` callback trampoline (P4.1).
        helper(&[ValType::I64, ValType::I64], &[ValType::I64]);
        // The `__async_step(gen: i64, sent: i64) -> i64` trampoline (P4.3 M4).
        helper(&[ValType::I64, ValType::I64], &[ValType::I64]);
    }
    // The two `new Promise` settler functions (P4.1) reuse the uniform function
    // type (type 1) — they are called through the funcref table like any function
    // value — so they add a FunctionSection entry but NO new type. Emitted only
    // when there are functions (the table exists and `new Promise` can occur).
    if has_funcs {
        functions.function(fn_type);
        functions.function(fn_type);
    }

    // Append the host import types LAST (S14.1/S14.3/S14.6) so they never disturb
    // the main/uniform/helper type indices above. All three are declared in the
    // IMPORT section (not the FunctionSection), so each adds a type but no defined
    // function: `print(bits: i64, flag: i32) -> ()`,
    // `__convert(bits: i64, op: i32) -> i64`, and
    // `__method(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64`.
    let print_type = helper_type_base + 38;
    types.ty().function([ValType::I64, ValType::I32], []);
    let convert_type = helper_type_base + 39;
    types
        .ty()
        .function([ValType::I64, ValType::I32], [ValType::I64]);
    let method_type = helper_type_base + 40;
    types.ty().function(
        [ValType::I64, ValType::I32, ValType::I64, ValType::I64],
        [ValType::I64],
    );
    // The host promise import types (P4.1): `__promise(op: i32, a: i64, b: i64) ->
    // i64` and `__then(p: i64, on_f: i64, on_r: i64) -> i64`.
    let promise_type = helper_type_base + 41;
    types
        .ty()
        .function([ValType::I32, ValType::I64, ValType::I64], [ValType::I64]);
    let then_type = helper_type_base + 42;
    types
        .ty()
        .function([ValType::I64, ValType::I64, ValType::I64], [ValType::I64]);
    // The host wall-clock import type (P-series `Date`): `__now() -> f64`.
    let now_type = helper_type_base + 43;
    types.ty().function([], [ValType::F64]);

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
    )
}

/// Build the module's global section: the mutable bump-allocator heap pointer
/// (`$__heap_ptr`, index [`HEAP_PTR_GLOBAL`], starting at `heap_ptr_init` — the
/// first free offset after all static string-literal data so the allocator never
/// overwrites them), followed by the S15.1 exception-unwinding globals — the
/// pending flag (`$exc_pending`, i32) and the in-flight value (`$exc_value`, i64),
/// both zero-initialized. A `throw` sets the latter two; every throwing call site
/// checks the flag to unwind. Two further zero-initialized globals — the
/// pending-completion kind (`$cf_kind`, i32) and routed return value
/// (`$cf_value`, i64) — carry a `return`/`break`/`continue` through the `finally`
/// clauses it must run (S15.3).
pub(super) fn build_globals(heap_ptr_init: i32, module_cells: u32) -> GlobalSection {
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
    // The pending-completion globals (S15.3): the kind ($cf_kind, i32) and the
    // routed return value ($cf_value, i64), driving a return/break/continue
    // through the `finally` clauses it must run. Internal, not exported.
    let (cf_kind_ty, cf_kind_init) = mut_i32(0);
    globals.global(cf_kind_ty, &cf_kind_init);
    let (cf_val_ty, cf_val_init) = mut_i64();
    globals.global(cf_val_ty, &cf_val_init);
    // The `this`-binding carrier ($__this, i64, index THIS_GLOBAL) — P2.1. Init to
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
    // The module-cell pointers (i64, index `MODULE_CELL_GLOBAL_BASE + n`), set in
    // `main`'s prologue before any user code runs.
    for _ in 0..module_cells {
        let (ty, init) = mut_i64();
        globals.global(ty, &init);
    }
    globals
}

/// Build the data section from the string-literal offset map: one active segment
/// per literal, each a `[len:u32 LE][utf8]` blob written at its compile-time
/// offset, emitted in ascending offset order for deterministic output. Returns
/// `None` when there are no literals (so no section is added).
pub(super) fn emit_data_section(offsets: &HashMap<String, i32>) -> Option<DataSection> {
    if offsets.is_empty() {
        return None;
    }
    let mut segments: Vec<(&String, i32)> = offsets.iter().map(|(s, o)| (s, *o)).collect();
    segments.sort_by_key(|(_, offset)| *offset);
    let mut data = DataSection::new();
    let header_size = HEADER_SIZE as usize;
    for (text, offset) in segments {
        let mut blob = Vec::with_capacity(header_size + 4 + text.len());
        // S13.1 block header: the TYPE_STRING type byte at offset 0, then padding
        // up to HEADER_SIZE (a byte-for-byte match of a runtime-allocated string).
        blob.push(u8::try_from(TYPE_STRING).unwrap_or(0));
        blob.resize(header_size, 0);
        let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        blob.extend_from_slice(&len.to_le_bytes());
        blob.extend_from_slice(text.as_bytes());
        data.active(0, &ConstExpr::i32_const(offset), blob);
    }
    Some(data)
}

/// Collect every distinct string literal in `program` and assign each a
/// compile-time byte offset in linear memory.
///
/// Literals are packed contiguously from [`HEAP_BASE`] in first-appearance
/// order, each occupying `4 + byte_len` bytes (`[len:u32 LE][utf8]`). Identical
/// literals are deduplicated onto one offset. Returns the offset map plus the
/// first free offset after all literal data — the bump allocator's start (equal
/// to [`HEAP_BASE`] when there are no literals).
pub(super) fn collect_strings(program: &IrProgram) -> (HashMap<String, i32>, i32) {
    let mut order: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for stmt in &program.stmts {
        collect_stmt(stmt, &mut order, &mut seen);
    }
    for func in &program.functions {
        for stmt in &func.body {
            collect_stmt(stmt, &mut order, &mut seen);
        }
        // A generator state machine carries its code in segments, not `body`
        // (P4.2 M2): walk each segment's statements and its terminator value.
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
        // Each blob is `[type=0][pad×3][len:u32][utf8]` (S13.1 header): the block
        // header, the 4-byte length header, plus the byte length.
        let blob_len = HEADER_SIZE + 4 + i32::try_from(text.len()).unwrap_or(i32::MAX);
        next = next.saturating_add(blob_len);
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
            for s in INTERNAL_TYPE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
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
        // A generator `yield e` / `IsGenerator` carries only its operand (P4.2).
        | IrNode::Yield(operand)
        | IrNode::IsGenerator(operand)
        | IrNode::ConvertCall { arg: operand, .. }
        | IrNode::ObjectBuiltin { arg: operand, .. } => {
            collect_node(operand, order, seen);
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
                collect_node(callee, order, seen);
            }
            collect_node(args, order, seen);
        }
        IrNode::MathSpread { args, .. } => {
            collect_node(args, order, seen);
        }
        IrNode::Object(entries) => {
            // Each key is a static string (its `[len][utf8]` blob must live in
            // the data segment so a literal can point at it) and shares the same
            // interning/dedup as string literals; recurse into each value.
            for (key, value) in entries {
                if seen.insert(key.as_str()) {
                    order.push(key.as_str());
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
            // Seed the G2 runtime callee-is-a-function guard's branded TypeError
            // strings so their data-segment blobs exist when the guard throws.
            for s in INTERNAL_TYPE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
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
        IrNode::ObjectKey { obj, index } => {
            collect_node(obj, order, seen);
            collect_node(index, order, seen);
        }
        // An instance accessor (P2b): recurse into the getter/setter function values
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
                // A user-class test reads both prototypes via `__obj_get`; the
                // `@@prototype` key (and the class read's literals) are interned
                // by recursing into the stored `<class>["@@prototype"]` expr.
                InstanceOfKind::Class(class_proto) => collect_node(class_proto, order, seen),
                // An error test reads the hidden `@@errorkind` marker via
                // `__obj_get`, so its key must be interned even when the program
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
            collect_node(obj, order, seen);
            collect_node(key, order, seen);
        }
        IrNode::JsonStringify { arg } => {
            collect_node(arg, order, seen);
            // Seed the serializer's literal token strings (`null`/`true`/`false`
            // and the structural punctuation) so their `[len][utf8]` blocks exist
            // in the data segment for `__json_str` to splice via `__concat` (P3.4).
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
            collect_node(obj, order, seen);
            collect_node(index, order, seen);
            collect_node(value, order, seen);
        }
        IrNode::IndexOpSet {
            obj, index, rhs, ..
        } => {
            seed_nullish_strs(order, seen);
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
            // which the G2 guard protects — seed its TypeError strings too.
            for s in INTERNAL_TYPE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
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
            // The non-Date fallback lowers to a user method call (G2 guard) — seed
            // its TypeError strings too.
            for s in INTERNAL_TYPE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
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
        // The promise primitives (P4.1) carry only sub-expressions to walk.
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
        IrNode::PromiseCtor { executor, .. } => collect_node(executor, order, seen),
        // `AsyncSpawn` (P4.3 M4) wraps the inner `MakeGenerator` — walk it so the
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
        // receiver throws a branded `TypeError`, so seed its strings too (P4.2 M2).
        IrNode::GenResume { handle, sent } => {
            for key in ["value", "done"] {
                if seen.insert(key) {
                    order.push(key);
                }
            }
            for s in INTERNAL_TYPE_ERROR_STRS {
                if seen.insert(s) {
                    order.push(s);
                }
            }
            collect_node(handle, order, seen);
            collect_node(sent, order, seen);
        }
        // The `Date` builtins (P-series) carry no string literals; recurse into
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
