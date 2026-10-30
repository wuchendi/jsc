#[allow(clippy::wildcard_imports)]
use super::*;

/// Load the element-store base of the array handle whose `i32` base is on the
/// stack (its `elems` slot); element `i` is then the jsval at
/// `store + ELEMS_DATA_OFFSET + 8*i`.
pub(super) fn emit_array_elems(function: &mut Function) {
    function.instruction(&Instruction::I32Load(MemArg {
        offset: ARRAY_ELEMS_OFFSET,
        align: 2,
        memory_index: 0,
    }));
}

/// The [`MemArg`] of element `k` of an element store (the store base on the
/// stack, plus any `8*i` already added).
pub(super) fn elem_slot(k: u64) -> MemArg {
    MemArg {
        offset: ELEMS_DATA_OFFSET + 8 * k,
        align: 3,
        memory_index: 0,
    }
}

/// Push the element jsval in i64 local `elem` as an absent element reads: a
/// [`HOLE_BITS`] slot reads `undefined` (`elem ^ (elem == HOLE)`, since the
/// hole is `undefined | 1`); any other element is itself. An array element
/// read resolves a hole through the prototype chain first
/// ([`emit_hole_lookup`]); this alone serves the slots that are not elements of a
/// user-visible array (a spread argument list) and what follows that lookup.
pub(super) fn emit_elem_read(function: &mut Function, elem: u32) {
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Xor);
}

/// Store the raw element jsval on the stack — element `idx` (i32 local) of
/// the array whose i32 base is in local `arr` — into i64 local `elem`; a
/// [`HOLE_BITS`] slot is replaced by `__proto_elem(arr, idx)`, the element
/// inherited through the prototype chain (still [`HOLE_BITS`] when absent).
/// An inherited getter may throw: the exception propagates.
pub(super) fn emit_hole_lookup(
    function: &mut Function,
    elem: u32,
    arr: u32,
    idx: u32,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalTee(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::Call(helpers.proto_elem()));
    function.instruction(&Instruction::LocalSet(elem));
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Push i32 nonzero when the array handle whose i32 base is in local `arr` is
/// `SPARSE` (its header `flags` bit), else `0`.
pub(super) fn emit_is_sparse(function: &mut Function, arr: u32) {
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(ARRAY_SPARSE));
    function.instruction(&Instruction::I32And);
}

/// Push the raw element jsval `idx` (an i32 local below the length) of the
/// array whose i32 base is in local `arr`, a hole as [`HOLE_BITS`]: the store
/// slot of a dense array, `__sparse_get` (function index `sparse_get`) of a
/// `SPARSE` one. The store base is reloaded per access, since a callback may
/// grow the array (replacing its store).
pub(super) fn emit_elem_load(function: &mut Function, arr: u32, idx: u32, sparse_get: u32) {
    emit_is_sparse(function, arr);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::Call(sparse_get));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(arr));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::End);
}

/// The [`MemArg`] of an array handle's u32 `length`.
pub(super) fn array_len_slot() -> MemArg {
    MemArg {
        offset: ARRAY_LEN_OFFSET,
        align: 2,
        memory_index: 0,
    }
}

/// Emit an array literal `[e0, e1, ..]`, leaving the array jsval (single heap tag
/// `0b101`, type byte `TYPE_ARRAY`) on the stack. A non-empty literal first
/// builds its `cap = n` element store (`__alloc(8 + 8*n)`, `TYPE_ELEMS`, `cap`,
/// then each element jsval at `store + 8 + 8*k`), then the handle pointing at
/// it; an empty literal is `__new_array(0)` (no store).
///
/// The store base is kept on the operand stack (never in a persistent local)
/// while the elements are evaluated, so nested array literals compose: an
/// element expression may itself build an array, clobbering the transient i32
/// scratch locals, without disturbing this store's base sitting beneath it.
pub(super) fn emit_array_node(
    elems: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The two transient i32 scratch locals: `dst` duplicates the block base
    // before each write; `store` holds the finished store while the handle is
    // built (no element expression runs after that).
    let dst = scratch + 3;
    let store = scratch + 7;
    if elems.is_empty() {
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::Call(helpers.new_array()));
    } else {
        let len = i32::try_from(elems.len()).unwrap_or(i32::MAX);
        // store = __alloc(8 + 8*n) with its type byte and cap; left on the stack.
        function.instruction(&Instruction::I32Const(
            abi::ELEMS_DATA_OFFSET
                .cast_signed()
                .saturating_add(len.saturating_mul(8)),
        ));
        function.instruction(&Instruction::Call(helpers.alloc));
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(TYPE_ELEMS));
        function.instruction(&Instruction::I32Store8(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(len));
        function.instruction(&Instruction::I32Store(MemArg {
            offset: ELEMS_CAP_OFFSET,
            align: 2,
            memory_index: 0,
        }));
        // Each element jsval at store + 8 + 8*k, left-to-right.
        for (k, elem) in elems.iter().enumerate() {
            function.instruction(&Instruction::LocalTee(dst));
            function.instruction(&Instruction::LocalGet(dst));
            emit_node(elem, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Store(elem_slot(k as u64)));
        }
        // The handle: object prefix, `len = n`, `elems = store`.
        function.instruction(&Instruction::LocalSet(store));
        function.instruction(&Instruction::I32Const(ARRAY_BLOCK_SIZE));
        function.instruction(&Instruction::Call(helpers.alloc));
        function.instruction(&Instruction::LocalSet(dst));
        emit_object_prefix(
            function,
            dst,
            TYPE_ARRAY,
            intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Array), helpers),
        );
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(len));
        function.instruction(&Instruction::I32Store(array_len_slot()));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::LocalGet(store));
        function.instruction(&Instruction::I32Store(MemArg {
            offset: ARRAY_ELEMS_OFFSET,
            align: 2,
            memory_index: 0,
        }));
        function.instruction(&Instruction::LocalGet(dst));
    }

    // Rebox the handle base (on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an array literal with a spread `[0, ...a, 9]`, leaving the built
/// array's heap jsval on the stack. Because a spread source's length is only
/// known at runtime, the array is grown in place: a fresh empty array
/// (`__new_array(0)`), then each part is appended left-to-right — a plain
/// [`Item`] via `__array_push`, a [`Spread`] source's values and a [`List`]'s
/// elements via `__array_concat`.
///
/// A [`Spread`] source is `IterableToList` (design/symbol-strings.md §5.2):
/// on its guarded fast path (§5.3, [`builtins::emit_iter_classify`]) a clean
/// array is itself, a string its characters (`__str_to_array`) and a
/// generator its drained values (`__gen_to_array`); `arguments` is copied
/// by `Array.prototype.slice`; every other value runs the protocol
/// ([`ProtocolOp::ToList`], which throws `<x> is not iterable`).
///
/// The handle base is kept purely on the operand stack (both runtime helpers
/// take it as their deepest argument and return it), so a nested spread array
/// in any part composes without a persistent local.
///
/// [`Item`]: SpreadElem::Item
/// [`Spread`]: SpreadElem::Spread
/// [`List`]: SpreadElem::List
pub(super) fn emit_array_spread(
    parts: &[SpreadElem],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // base = a fresh empty array, left (i32) on the operand stack.
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.new_array()));

    // Append each part; `base` stays on the stack, the source/value sits above it.
    for part in parts {
        match part {
            SpreadElem::Item(value) => {
                // base = __array_push(base, value).
                emit_node(value, function, scratch, strings, helpers);
                function.instruction(&Instruction::Call(helpers.array_push));
            }
            SpreadElem::List(list) => {
                // base = __array_concat(base, list).
                emit_node(list, function, scratch, strings, helpers);
                function.instruction(&Instruction::I32WrapI64);
                function.instruction(&Instruction::Call(helpers.array_concat));
                if helpers.has_accessors {
                    emit_propagate_if_pending(function, helpers);
                }
            }
            SpreadElem::Spread(source) => {
                // base = __array_concat(base, IterableToList(source)); `base`
                // (i32) stays beneath on the operand stack throughout.
                let value = scratch + 22;
                emit_node(source, function, scratch, strings, helpers);
                function.instruction(&Instruction::LocalSet(value));
                builtins::emit_iter_classify(function, value, false, scratch + 3, helpers);
                function.instruction(&Instruction::LocalSet(scratch + 26));
                emit_spread_list(value, scratch + 26, function, helpers);
                function.instruction(&Instruction::I32WrapI64);
                function.instruction(&Instruction::Call(helpers.array_concat));
                // A getter inherited by a hole may have thrown.
                if helpers.has_accessors {
                    emit_propagate_if_pending(function, helpers);
                }
            }
        }
    }

    // Rebox the finished block's base offset (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Push the array of values a spread of the jsval in i64 `value` appends,
/// by its record state in i64 `state` ([`builtins::emit_iter_classify`]):
/// the array itself, a string's characters, a generator's drained values,
/// a copy of `arguments`, else the protocol's list. A throw propagates.
fn emit_spread_list(value: u32, state: u32, function: &mut Function, helpers: Helpers<'_>) {
    let state_is = |function: &mut Function, kind: f64| {
        function.instruction(&Instruction::LocalGet(state));
        function.instruction(&Instruction::I64Const(kind.to_bits().cast_signed()));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        function.instruction(&Instruction::LocalGet(value));
    };
    let (array, string, generator, arguments) = (
        builtins::ST_ARRAY,
        builtins::ST_STRING,
        builtins::ST_GEN,
        builtins::ST_ARGS,
    );
    state_is(function, array);
    function.instruction(&Instruction::Else);
    state_is(function, string);
    function.instruction(&Instruction::Call(helpers.str_to_array));
    function.instruction(&Instruction::Else);
    state_is(function, generator);
    function.instruction(&Instruction::Call(helpers.gen_to_array));
    function.instruction(&Instruction::Else);
    for _ in 0..3 {
        enter_frame(helpers);
    }
    let builtin = |function: &mut Function, builtin: Builtin| {
        for _ in 0..helpers.max_args {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        function.instruction(&Instruction::LocalGet(value));
        function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
        function.instruction(&Instruction::I64Const(0));
        emit_argc(0, function);
        function.instruction(&Instruction::Call(helpers.builtin(builtin)));
    };
    function.instruction(&Instruction::LocalGet(state));
    function.instruction(&Instruction::I64Const(
        f64::to_bits(arguments).cast_signed(),
    ));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    builtin(function, Builtin::ArraySlice);
    function.instruction(&Instruction::Else);
    builtin(function, Builtin::Protocol(ProtocolOp::ToList));
    function.instruction(&Instruction::End);
    emit_propagate_if_pending(function, helpers);
    for _ in 0..3 {
        leave_frame(helpers);
    }
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Emit `array.push(value)`, leaving the new length (a number jsval) on
/// the stack: `__array_push` appends `value` to the array handle read from
/// `local` in place (the handle never moves, so every alias observes the
/// growth) and returns the handle, whose `length` is then read back.
pub(super) fn emit_array_push_node(
    local: u32,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit `array.pop()`, leaving the removed last element (or `undefined`
/// for an empty array) on the stack: the array handle read from `local` is
/// shrunk IN PLACE — the element at the old last index is read first (a hole
/// through the prototype chain), then `length` drops by one (a `SPARSE`
/// array reads the element through `__sparse_get` and shrinks through
/// `__array_set_length`, which drops it from the property table).
pub(super) fn emit_array_pop_node(
    local: u32,
    function: &mut Function,
    scratch: u32,
    helpers: Helpers<'_>,
) {
    // Transient scratch locals: the handle base and the new length (i32), and
    // the removed element (i64).
    let base = scratch + 3;
    let new_len = scratch + 7;
    let removed = scratch + 4;

    // base = wrap(array_handle).
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // len == 0 ? undefined : pop the last element.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    // removed = element new_len = len - 1 (a hole read through the chain).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(new_len));
    emit_elem_load(function, base, new_len, helpers.sparse_get());
    emit_hole_lookup(function, removed, base, new_len, helpers);
    // A SPARSE array: the `length` write `len - 1` drops that element; a
    // dense one writes the new length back into the handle.
    emit_is_sparse(function, base);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::Call(helpers.array_set_length()));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::I32Store(array_len_slot()));
    function.instruction(&Instruction::End);
    emit_elem_read(function, removed);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Push the raw element jsval at `arr[idx]` (a hole as [`HOLE_BITS`]) onto the
/// stack via [`emit_elem_load`]. `arr`/`idx` are the array-handle-base /
/// loop-index i32 scratch locals of an [`emit_array_hof`] loop.
pub(super) fn emit_hof_load_elem(
    function: &mut Function,
    arr: u32,
    idx: u32,
    helpers: Helpers<'_>,
) {
    emit_elem_load(function, arr, idx, helpers.sparse_get());
}

/// Push the loop index `idx` (an i32 scratch local) as a number jsval — its
/// `f64` bit pattern in the i64 carrier.
fn emit_hof_index(function: &mut Function, idx: u32) {
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit the trailing half of a higher-order callback invocation: pad the
/// remaining positional slots to the shared funcref arity with `undefined`, set
/// `this` (from i64 local `this`, else `undefined`), push the callback's own
/// environment (at `+FUNC_ENV_OFFSET`) as the env-last argument, the argument
/// count `argc`, and its funcref-table index (at `+FUNC_INDEX_OFFSET`), and
/// `call_indirect`. The real positional args (already pushed by the caller)
/// number `pushed`, at most `helpers.max_args`. Leaves the callback's boxed
/// jsval result.
fn emit_hof_invoke(
    function: &mut Function,
    helpers: Helpers<'_>,
    cb: u32,
    this: Option<u32>,
    pushed: u32,
    argc: HofArgc,
) {
    for _ in pushed..helpers.max_args {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // The real positional args are already on the stack, so setting the
    // carrier here (stack-neutral) does not disturb them.
    match this {
        Some(this) => {
            function.instruction(&Instruction::LocalGet(this));
            function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
        }
        None => emit_reset_this(function),
    }
    function.instruction(&Instruction::LocalGet(cb));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI): the arguments the callback receives, so
    // a rest-parameter callback collects exactly those.
    match argc {
        HofArgc::Fixed(n) => emit_argc(n as usize, function),
        HofArgc::Spilled { all, pushed, spill } => {
            emit_argc(all as usize, function);
            emit_argc(pushed as usize, function);
            function.instruction(&Instruction::LocalGet(spill));
            function.instruction(&Instruction::I64Const(0));
            function.instruction(&Instruction::I64Ne);
            function.instruction(&Instruction::Select);
        }
    }
    function.instruction(&Instruction::LocalGet(cb));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // A higher-order callback can throw; unwind if it left an exception pending.
    // `return` exits the whole function regardless of the enclosing HOF loop's
    // block nesting, so an in-flight exception cleanly abandons the loop.
    emit_propagate_if_pending(function, helpers);
}

/// The argument count [`emit_hof_invoke`] passes.
#[derive(Clone, Copy)]
enum HofArgc {
    /// Always `n`.
    Fixed(u32),
    /// `all` when the i64 local `spill` holds the published spread argument
    /// array (nonzero), else the `pushed` positional ones.
    Spilled { all: u32, pushed: u32, spill: u32 },
}

/// Seed the i64 local `spill` before an [`emit_array_hof`] loop whose
/// callback (base in i32 local `cb`) is passed `all > K` arguments: a fresh
/// `all`-element array that every invocation refills and publishes as the
/// spread argument array, so the arguments past the `K` positional slots
/// reach a callback gathering them (a rest parameter, `arguments`, a variadic
/// or bound builtin; each copies them out on entry, so one array serves the
/// whole loop). When no user function gathers its arguments only a builtin
/// callee (a funcref slot past the user functions and the settlers) can, so
/// any other callback keeps `spill = 0` and receives the `K` positional
/// arguments alone, as if passed directly.
fn emit_hof_spill_seed(
    function: &mut Function,
    helpers: Helpers<'_>,
    cb: u32,
    spill: u32,
    all: u32,
) {
    let alloc = |function: &mut Function| {
        function.instruction(&Instruction::I32Const(all.cast_signed()));
        function.instruction(&Instruction::Call(helpers.new_array()));
        function.instruction(&Instruction::I64ExtendI32U);
        function.instruction(&Instruction::I64Const(HEAP_BITS));
        function.instruction(&Instruction::I64Or);
    };
    if helpers.gathers_arguments {
        alloc(function);
    } else {
        let first_builtin = helpers.builtin_slot(Builtin::ALL[0]);
        function.instruction(&Instruction::LocalGet(cb));
        function.instruction(&Instruction::I32Load(MemArg {
            offset: FUNC_INDEX_OFFSET,
            align: 2,
            memory_index: 0,
        }));
        function.instruction(&Instruction::I32Const(first_builtin.cast_signed()));
        function.instruction(&Instruction::I32GeU);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        alloc(function);
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::I64Const(0));
        function.instruction(&Instruction::End);
    }
    function.instruction(&Instruction::LocalSet(spill));
}

/// The `TypeError` of `reduce` over no element without an initial value.
pub(super) const REDUCE_EMPTY: CalleeErrorText<'static> = [
    "Reduce of empty array with no initial value",
    "TypeError: Reduce of empty array with no initial value",
];

/// Emit an array higher-order method ([`IrNode::ArrayHof`]), leaving the
/// method's result jsval on the stack. The receiver is evaluated once into
/// the i32 base scratch local `arr` (not rebound), its length on entry into
/// `len`, the callback's base into `cb` (after its `IsCallable` check) and the
/// `this_arg` into `this`; a counted loop over `0..len` then invokes the
/// callback per element still present (below the current length, not a hole)
/// through the uniform `call_indirect` ABI via [`emit_hof_invoke`], passing
/// `(element, index, array)` (`reduce`: `(acc, element, index, array)`): the
/// first `K` in the positional slots, all of them through the spread argument
/// array when there are more than `K` and the callback may gather them
/// ([`emit_hof_spill_seed`]):
///
/// - `Map` fills a fresh same-length array `res[i] = fn(..)`, a skipped index
///   staying a hole.
/// - `Filter` grows a fresh array in place via `__array_push` for JS-truthy
///   results.
/// - `Reduce` folds `acc = fn(acc, ..)` from `init`, or without one from the
///   first present element (`seeded` tracks it; none throws [`REDUCE_EMPTY`]).
/// - `ForEach` discards each result and yields `undefined`.
///
/// The sub-expressions are fully evaluated before the loop seeds the HOF
/// scratch locals (`scratch+8`..`scratch+14`), so a nested HOF in any of them
/// completes and frees those locals first — the loop body itself only reads
/// memory and `call_indirect`s, never re-entering an inner HOF at this frame.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn emit_array_hof(
    op: ArrayHofOp,
    array: &IrNode,
    callback: &IrNode,
    init: Option<&IrNode>,
    this_arg: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let arr = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let cb = scratch + 11;
    // The result array of map/filter; reduce's `seeded` flag.
    let res = scratch + 12;
    let seeded = res;
    let acc = scratch + 13;
    let this = scratch + 14;
    // The visited element: the store slot, or (for a hole) the inherited one.
    let elem = scratch + 16;
    // The spread argument array of a callback passed more than `K` arguments.
    let spill = scratch + 15;
    let all = if op == ArrayHofOp::Reduce { 4 } else { 3 };
    let spilled = all > helpers.max_args;

    // Prologue: receiver base -> `arr`, its length -> `len`, then the
    // callback (IsCallable, DF4: a non-function throws before any iteration,
    // even over an empty array; a function literal needs no check), `this`
    // and the initial accumulator.
    emit_node(array, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalSet(len));
    emit_node(callback, function, scratch, strings, helpers);
    if !is_static_function(callback) {
        function.instruction(&Instruction::LocalSet(scratch));
        emit_callable_guard(
            function,
            EXPRESSION_NOT_A_FUNCTION,
            scratch,
            scratch,
            strings,
            helpers,
        );
        function.instruction(&Instruction::LocalGet(scratch));
    }
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(cb));
    if let Some(this_arg) = this_arg {
        emit_node(this_arg, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(this));
    }
    let this = this_arg.map(|_| this);
    if spilled {
        emit_hof_spill_seed(function, helpers, cb, spill, all);
    }
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(idx));

    // Pre-loop setup: allocate the result array (map/filter) or seed the
    // accumulator (reduce).
    match op {
        ArrayHofOp::Map => {
            // res = __new_array(len): a same-length array with a `cap = len` store.
            function.instruction(&Instruction::LocalGet(len));
            function.instruction(&Instruction::Call(helpers.new_array()));
            function.instruction(&Instruction::LocalSet(res));
        }
        ArrayHofOp::Filter => {
            // res = a fresh empty array (grown in place per truthy element by
            // __array_push).
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::Call(helpers.new_array()));
            function.instruction(&Instruction::LocalSet(res));
        }
        ArrayHofOp::Reduce => {
            if let Some(init) = init {
                emit_node(init, function, scratch, strings, helpers);
                function.instruction(&Instruction::LocalSet(acc));
            } else {
                function.instruction(&Instruction::I32Const(0));
                function.instruction(&Instruction::LocalSet(seeded));
            }
        }
        ArrayHofOp::ForEach => {}
    }
    // Push the callback's argument `j` of `(element, index, array)`.
    let callback_arg = |function: &mut Function, j: u32| match j {
        0 => {
            function.instruction(&Instruction::LocalGet(elem));
        }
        1 => emit_hof_index(function, idx),
        _ => {
            function.instruction(&Instruction::LocalGet(arr));
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(HEAP_BITS));
            function.instruction(&Instruction::I64Or);
        }
    };
    // Push the positional arguments (`reduce`'s accumulator is pushed by the
    // caller), refill and publish the spread argument array when spilled,
    // and invoke.
    let call = |function: &mut Function, before: u32| {
        let n = all.min(helpers.max_args);
        for j in before..n {
            callback_arg(function, j - before);
        }
        if !spilled {
            emit_hof_invoke(function, helpers, cb, this, n, HofArgc::Fixed(n));
            return;
        }
        if !helpers.gathers_arguments {
            function.instruction(&Instruction::LocalGet(spill));
            function.instruction(&Instruction::I64Const(0));
            function.instruction(&Instruction::I64Ne);
            function.instruction(&Instruction::If(BlockType::Empty));
        }
        for j in 0..all {
            function.instruction(&Instruction::LocalGet(spill));
            function.instruction(&Instruction::I32WrapI64);
            emit_array_elems(function);
            if j < before {
                function.instruction(&Instruction::LocalGet(acc));
            } else {
                callback_arg(function, j - before);
            }
            function.instruction(&Instruction::I64Store(elem_slot(u64::from(j))));
        }
        function.instruction(&Instruction::LocalGet(spill));
        function.instruction(&Instruction::GlobalSet(SPREAD_ARGS_GLOBAL));
        let argc = if helpers.gathers_arguments {
            HofArgc::Fixed(all)
        } else {
            function.instruction(&Instruction::End);
            HofArgc::Spilled {
                all,
                pushed: n,
                spill,
            }
        };
        emit_hof_invoke(function, helpers, cb, this, n, argc);
    };

    // Counted loop: `while (idx < len) { <body>; idx++ }`.
    function.instruction(&Instruction::Block(BlockType::Empty));
    enter_frame(helpers); // outer block
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers); // loop — the callback's throw check sits inside both
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // Visit a present element only: below the current length (a callback may
    // shrink the array) and either in the store or, for a hole, inherited
    // (`HasProperty` then `Get` through the prototype chain).
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    enter_frame(helpers);
    emit_hof_load_elem(function, arr, idx, helpers);
    emit_hole_lookup(function, elem, arr, idx, helpers);
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);

    match op {
        ArrayHofOp::Map => {
            // res[idx] = callback(..). The slot address (res.elems + 8*idx) is
            // pushed first and survives beneath the call_indirect, which is
            // stack-balanced (leaves one i64); `res` is private to the loop,
            // so its store never moves.
            function.instruction(&Instruction::LocalGet(res));
            emit_array_elems(function);
            function.instruction(&Instruction::LocalGet(idx));
            function.instruction(&Instruction::I32Const(8));
            function.instruction(&Instruction::I32Mul);
            function.instruction(&Instruction::I32Add);
            call(function, 0);
            function.instruction(&Instruction::I64Store(elem_slot(0)));
        }
        ArrayHofOp::Filter => {
            // if truthy(callback(..)) { __array_push(res, element) }.
            call(function, 0);
            function.instruction(&Instruction::LocalSet(acc));
            emit_cond(&IrNode::Local(acc), function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::LocalGet(elem));
            function.instruction(&Instruction::Call(helpers.array_push));
            function.instruction(&Instruction::Drop);
            function.instruction(&Instruction::End);
        }
        ArrayHofOp::Reduce => {
            // acc = callback(acc, ..), or the first present element seeds it.
            if init.is_none() {
                function.instruction(&Instruction::LocalGet(seeded));
                function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                enter_frame(helpers);
            }
            if helpers.max_args > 0 {
                function.instruction(&Instruction::LocalGet(acc));
            }
            call(function, 1);
            if init.is_none() {
                function.instruction(&Instruction::Else);
                function.instruction(&Instruction::LocalGet(elem));
                function.instruction(&Instruction::I32Const(1));
                function.instruction(&Instruction::LocalSet(seeded));
                leave_frame(helpers);
                function.instruction(&Instruction::End);
            }
            function.instruction(&Instruction::LocalSet(acc));
        }
        ArrayHofOp::ForEach => {
            // callback(..) for side effects; discard the result.
            call(function, 0);
            function.instruction(&Instruction::Drop);
        }
    }
    if op == ArrayHofOp::Map {
        // A skipped index stays a hole.
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::LocalGet(res));
        emit_array_elems(function);
        function.instruction(&Instruction::LocalGet(idx));
        function.instruction(&Instruction::I32Const(8));
        function.instruction(&Instruction::I32Mul);
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::I64Const(HOLE_BITS));
        function.instruction(&Instruction::I64Store(elem_slot(0)));
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // idx++ and continue.
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(idx));
    function.instruction(&Instruction::Br(0));
    leave_frame(helpers); // loop
    function.instruction(&Instruction::End); // loop
    leave_frame(helpers); // block
    function.instruction(&Instruction::End); // block

    if op == ArrayHofOp::Reduce && init.is_none() {
        function.instruction(&Instruction::LocalGet(seeded));
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        emit_throw_not_a_function(function, REDUCE_EMPTY, scratch, strings, helpers);
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }

    // Result: the fresh array (map/filter), the accumulator (reduce), or
    // `undefined` (forEach).
    match op {
        ArrayHofOp::Map | ArrayHofOp::Filter => {
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(HEAP_BITS));
            function.instruction(&Instruction::I64Or);
        }
        ArrayHofOp::Reduce => {
            function.instruction(&Instruction::LocalGet(acc));
        }
        ArrayHofOp::ForEach => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    }
}
