#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit an object literal `{k0: e0, ..}`, leaving the object jsval (single heap
/// tag `0b101`, type byte `TYPE_OBJECT`) on the stack. `__alloc(HEADER_SIZE + 4 +
/// 16*n)` reserves the block; the type byte, the u32 `n` header, then each entry's
/// key heap-string pointer jsval at `dst + HEADER_SIZE + 4 + 16*k` and value jsval
/// at `+8` are written. Keys reuse the interned static-string data via `HEAP_BITS
/// | key_offset`. Mirrors [`emit_array_node`]'s alloc→write→rebox shape,
/// keeping `dst` on the operand stack so nested objects/arrays in the value
/// expressions compose.
pub(super) fn emit_object_node(
    entries: &[(String, IrNode)],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The i32 scratch local (declared just past the three i64 scratch locals) is
    // used only transiently, to duplicate `dst` before each write.
    let dst = scratch + 3;
    // Block header: the u32 `n` header sits at `+HEADER_SIZE`; entry `k`'s
    // key at `+HEADER_SIZE+4+16*k` and value at `+8` from the key.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let payload = (HEADER_SIZE + 4) as u64;
    let n = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let size = HEADER_SIZE + 4 + 16 * i32::try_from(entries.len()).unwrap_or(i32::MAX);

    // dst = __alloc(HEADER_SIZE + 4 + 16*n); leave dst (i32) on the stack.
    function.instruction(&Instruction::I32Const(size));
    function.instruction(&Instruction::Call(helpers.alloc));

    // Write the TYPE_OBJECT type byte at dst (offset 0), keeping dst on the stack.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Store8(type_byte));

    // Store the u32 count header at dst + HEADER_SIZE, keeping dst on the stack.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(n.cast_signed()));
    function.instruction(&Instruction::I32Store(header));

    // Store each (key_ptr, value) entry: key at dst + HEADER_SIZE + 4 + 16*k,
    // value at +8. `dst` stays on the stack throughout.
    for (k, (key, value)) in entries.iter().enumerate() {
        // Key heap-string pointer jsval: HEAP_BITS OR the interned offset.
        let offset = strings.get(key.as_str()).copied().unwrap_or(HEAP_BASE);
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I64Const(HEAP_BITS | i64::from(offset)));
        function.instruction(&Instruction::I64Store(MemArg {
            offset: payload + 16 * k as u64,
            align: 0,
            memory_index: 0,
        }));
        // Value jsval at the +8 slot.
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        emit_node(value, function, scratch, strings, helpers);
        function.instruction(&Instruction::I64Store(MemArg {
            offset: payload + 16 * k as u64 + 8,
            align: 0,
            memory_index: 0,
        }));
    }

    // Rebox the object block's base offset (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit a [`crate::ir::IrNode::MakeAccessor`] — build a `TYPE_ACCESSOR` heap block
/// holding the getter and setter FUNCTION VALUES, leaving its heap jsval on
/// the stack. An [`ACCESSOR_BLOCK_SIZE`] block is bump-allocated; the type byte,
/// the getter jsval (at [`ACCESSOR_GET_OFFSET`]), and the setter jsval (at
/// [`ACCESSOR_SET_OFFSET`]) are written, then the base is reboxed as a heap jsval.
///
/// Mirrors [`emit_object_node`]'s alloc→write→rebox discipline: `dst` rides the
/// operand stack and is re-`tee`d before each write, so building the getter/setter
/// closure values (which reuse the `+3` allocation scratch for their OWN blocks)
/// cannot corrupt the accessor block base. A `MakeAccessor` only ever appears as a
/// value inside the class prototype object, so the two closure builds are the only
/// nested emits and this stack discipline suffices.
pub(super) fn emit_make_accessor_node(
    getter: &IrNode,
    setter: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let get_slot = MemArg {
        offset: ACCESSOR_GET_OFFSET,
        align: 0,
        memory_index: 0,
    };
    let set_slot = MemArg {
        offset: ACCESSOR_SET_OFFSET,
        align: 0,
        memory_index: 0,
    };

    // dst = __alloc(ACCESSOR_BLOCK_SIZE); write the TYPE_ACCESSOR byte, keeping dst
    // on the operand stack.
    function.instruction(&Instruction::I32Const(ACCESSOR_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ACCESSOR));
    function.instruction(&Instruction::I32Store8(type_byte));

    // Getter jsval slot. Re-`tee` dst from the stack, push a copy for the store
    // address, evaluate the getter (may clobber the `dst` LOCAL via its own alloc —
    // harmless, the stack copy is the live address), then store.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    emit_node(getter, function, scratch, strings, helpers);
    function.instruction(&Instruction::I64Store(get_slot));

    // Setter jsval slot, same discipline.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    emit_node(setter, function, scratch, strings, helpers);
    function.instruction(&Instruction::I64Store(set_slot));

    // Rebox the block base (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an `Object.keys/values/entries(arg)` collection built-in, leaving
/// the fresh growable Array's heap jsval on the stack. `arg` is lowered and
/// unwrapped to its object block base offset, passed to the matching
/// `__obj_keys`/`__obj_values`/`__obj_entries` runtime helper, and its returned
/// array base is reboxed as a heap jsval. Mirrors [`emit_object_rest_node`]'s
/// wrap→call→rebox shape.
pub(super) fn emit_object_builtin_node(
    op: ObjectBuiltinOp,
    arg: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let helper = match op {
        ObjectBuiltinOp::Keys => helpers.obj_keys,
        ObjectBuiltinOp::Values => helpers.obj_values,
        ObjectBuiltinOp::Entries => helpers.obj_entries,
    };
    // obj_ptr = i32.wrap(arg jsval).
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helper));
    // Rebox the returned array block's base offset as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit a `key in obj` membership test, leaving a `true`/`false` jsval on
/// the stack. `obj` and `key` are each lowered and unwrapped to their block base
/// offsets, passed to the `__obj_has` runtime helper (own-key scan), and its `1`/
/// `0` i32 result is boxed into a boolean jsval (`FALSE_BITS | bit`, matching the
/// `!operand` boxing).
pub(super) fn emit_object_has_node(
    key: &IrNode,
    obj: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // obj_ptr = i32.wrap(obj jsval); key_ptr = i32.wrap(key jsval).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    emit_node(key, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.obj_has));
    // Box the i32 `1`/`0` into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an `x instanceof RHS` identity test, leaving a `true`/`false`
/// jsval on the stack. The operand `value` is evaluated once into a dedicated
/// high scratch slot and gated by [`emit_is_object`]: only a heap OBJECT can be
/// an instance, so a non-heap or non-object operand (number, `null`, array,
/// `Map`, …) yields `false` without ever reading a bogus type byte. For an
/// object the per-`kind` test runs: a **user class** compares the receiver's
/// `@@prototype` (via `__obj_get`) against the class's prototype pointer (a
/// single pointer-identity hop); a **builtin error** fetches the hidden
/// `@@errorkind` marker and matches its presence (`Error`) or exact kind
/// (`TypeError`/`RangeError`). `__obj_get`'s own-key scan finds the receiver's
/// own `@@prototype`/`@@errorkind` before any prototype walk, so the test is
/// exact.
#[allow(clippy::too_many_lines)] // straight-line per-kind emit, not branching depth
pub(super) fn emit_instanceof_node(
    value: &IrNode,
    kind: &InstanceOfKind,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // Evaluate the operand once and stash it in a dedicated high slot (the child
    // emit reuses the low scratch slots, matching `emit_index_node`).
    let v = scratch + 17;
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(v));

    // The runtime-TYPE-BYTE builtins test the receiver's heap kind directly
    // (independent of the byte-2 object gate the class/error kinds share):
    // `Array`/`Function`/`Date` match their own byte; `Object` matches ANY heap
    // pointer whose byte is not `TYPE_STRING` (a primitive string is not an object).
    match kind {
        InstanceOfKind::Array => return emit_type_byte_instanceof(function, v, TYPE_ARRAY, false),
        InstanceOfKind::Function => {
            return emit_type_byte_instanceof(function, v, TYPE_FUNCTION, false);
        }
        InstanceOfKind::Date => return emit_type_byte_instanceof(function, v, TYPE_DATE, false),
        InstanceOfKind::Object => {
            return emit_type_byte_instanceof(function, v, TYPE_STRING, true);
        }
        InstanceOfKind::Class(_)
        | InstanceOfKind::Error
        | InstanceOfKind::TypeError
        | InstanceOfKind::RangeError => {}
    }

    emit_is_object(function, v);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    match kind {
        InstanceOfKind::Class(class_proto) => {
            // Walk the receiver's `@@prototype` chain for pointer identity with the
            // class's shared prototype: `true` iff some link is the SAME block (a
            // single hop under no-`extends`, more under `extends`). `__obj_get` reads
            // an OWN `@@prototype` slot (no fallback recursion), so each step advances
            // exactly one link and the base class (no link) reads `undefined`, ending
            // the walk. `cur`/`tgt` are dedicated high slots (`+19`/`+20`).
            let cur = scratch + 19;
            let tgt = scratch + 20;
            let proto_offset = strings
                .get(crate::abi::CLASS_PROTOTYPE_KEY)
                .copied()
                .unwrap_or(HEAP_BASE);
            // cur = recv["@@prototype"] (read while `v` is intact).
            function.instruction(&Instruction::LocalGet(v));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::I32Const(proto_offset));
            function.instruction(&Instruction::Call(helpers.obj_get));
            function.instruction(&Instruction::LocalSet(cur));
            // tgt = <class>["@@prototype"] (its emit may reuse `+17`/`+18`; `v` is
            // already consumed).
            emit_node(class_proto, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(tgt));
            // Loop: 1 iff `cur` reaches `tgt`, 0 iff the chain runs out (non-object).
            function.instruction(&Instruction::Block(BlockType::Result(ValType::I32)));
            function.instruction(&Instruction::Loop(BlockType::Empty));
            emit_is_object(function, cur);
            function.instruction(&Instruction::I32Eqz);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::Br(2)); // -> block result: not an instance
            function.instruction(&Instruction::End);
            function.instruction(&Instruction::LocalGet(cur));
            function.instruction(&Instruction::LocalGet(tgt));
            function.instruction(&Instruction::I64Eq);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::I32Const(1));
            function.instruction(&Instruction::Br(2)); // -> block result: match
            function.instruction(&Instruction::End);
            // cur = cur["@@prototype"] — the next link up the chain.
            function.instruction(&Instruction::LocalGet(cur));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::I32Const(proto_offset));
            function.instruction(&Instruction::Call(helpers.obj_get));
            function.instruction(&Instruction::LocalSet(cur));
            function.instruction(&Instruction::Br(0)); // continue the loop
            function.instruction(&Instruction::End); // loop
            function.instruction(&Instruction::I32Const(0)); // unreachable fall-through
            function.instruction(&Instruction::End); // block -> i32 on the stack
        }
        error_kind => {
            // marker = __obj_get(v_ptr, "@@errorkind"_ptr); absent -> undefined.
            function.instruction(&Instruction::LocalGet(v));
            function.instruction(&Instruction::I32WrapI64);
            let key_offset = strings
                .get(crate::abi::ERROR_KIND_KEY)
                .copied()
                .unwrap_or(HEAP_BASE);
            function.instruction(&Instruction::I32Const(key_offset));
            function.instruction(&Instruction::Call(helpers.obj_get));
            match error_kind {
                // `instanceof Error`: any branded error (marker present).
                InstanceOfKind::Error => {
                    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
                    function.instruction(&Instruction::I64Ne);
                }
                // Exact kind: the marker strictly equals the kind's number jsval.
                InstanceOfKind::TypeError => {
                    function.instruction(&Instruction::I64Const(
                        crate::abi::ERROR_KIND_TYPE.to_bits().cast_signed(),
                    ));
                    function.instruction(&Instruction::I64Eq);
                }
                InstanceOfKind::RangeError => {
                    function.instruction(&Instruction::I64Const(
                        crate::abi::ERROR_KIND_RANGE.to_bits().cast_signed(),
                    ));
                    function.instruction(&Instruction::I64Eq);
                }
                // Unreachable: `Class` is handled by the outer arm above, and the
                // runtime-TYPE-BYTE kinds return before this `is_object` gate.
                InstanceOfKind::Class(_)
                | InstanceOfKind::Array
                | InstanceOfKind::Function
                | InstanceOfKind::Date
                | InstanceOfKind::Object => {
                    function.instruction(&Instruction::Drop);
                    function.instruction(&Instruction::I32Const(0));
                }
            }
        }
    }
    // Box the i32 `1`/`0` into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::Else);
    // A non-object operand is never an instance.
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::End);
}

/// Emit a runtime-TYPE-BYTE `instanceof` test for the operand already stashed in
/// local `v`, leaving a TRUE/FALSE jsval on the stack. `true` iff `v` is a heap
/// pointer whose type byte equals `type_byte` — or, when `negate` is set, whose
/// type byte does NOT equal `type_byte` (the `instanceof Object` case with
/// `type_byte == TYPE_STRING`, matching any heap pointer that is not a primitive
/// string). The type-byte read is gated behind [`emit_is_pointer`] so a
/// non-pointer operand (whose wrapped low bits would be a bogus offset) is simply
/// never an instance.
fn emit_type_byte_instanceof(function: &mut Function, v: u32, type_byte: i32, negate: bool) {
    emit_is_pointer(function, v);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    emit_load_type_byte(function, v);
    function.instruction(&Instruction::I32Const(type_byte));
    if negate {
        function.instruction(&Instruction::I32Ne);
    } else {
        function.instruction(&Instruction::I32Eq);
    }
    // Box the i32 `1`/`0` into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::Else);
    // A non-pointer operand is never an instance.
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::End);
}

/// Emit a `delete obj.k` property removal, leaving a `true` jsval on the
/// stack (`delete` always evaluates to `true` here). `obj` and `key` are each
/// lowered and unwrapped to their block base offsets and passed to the
/// `__obj_delete` runtime helper (which compacts the entry out on a match, or is a
/// no-op on a miss); the `true` result is then pushed.
pub(super) fn emit_object_delete_node(
    obj: &IrNode,
    key: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // obj_ptr = i32.wrap(obj jsval); key_ptr = i32.wrap(key jsval).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    emit_node(key, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.obj_delete));
    // `delete` yields `true`.
    function.instruction(&Instruction::I64Const(TRUE_BITS));
}

/// Emit a `JSON.stringify(arg)` call, leaving the serialized heap-string
/// jsval on the stack (or the `undefined` jsval when `arg` is itself `undefined`
/// or a function value — both stringify to `undefined`, not a string, in JS). The
/// argument is lowered once and stashed in the first i64 scratch slot so the
/// undefined/function guard can inspect it without re-evaluating; a serializable
/// value is handed to the in-wasm `__json_str` helper and its returned string base
/// is reboxed as a heap jsval.
pub(super) fn emit_json_stringify_node(
    arg: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));
    // `undefined`/function values are not serializable at the top level (they
    // yield `undefined`); everything else serializes to a heap string.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    emit_is_function(function, scratch);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::Call(helpers.json_str));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::End);
}

/// Emit a `new Map()` / `new Set()` construction, leaving the fresh handle
/// jsval on the stack. Two blocks are bump-allocated: an empty growable backing
/// array (`TYPE_ARRAY`, len 0) and the handle itself (`TYPE_MAP`/`TYPE_SET`), whose
/// `size` header starts at 0 and whose `entries` slot holds the backing array's
/// heap jsval. The handle jsval never moves afterward — growth reallocs the backing
/// and rewrites the `entries` slot in place.
pub(super) fn emit_map_new_node(
    set: bool,
    function: &mut Function,
    scratch: u32,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let earr = scratch + 7;
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let entries_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };

    // Empty backing array: __alloc(HEADER_SIZE + 4); TYPE_ARRAY byte; len = 0.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(earr));
    function.instruction(&Instruction::LocalGet(earr));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalGet(earr));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(header));

    // Handle: __alloc(MAP_BLOCK_SIZE); type byte; size = 0; entries = rebox(earr).
    function.instruction(&Instruction::I32Const(MAP_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(if set {
        TYPE_SET
    } else {
        TYPE_MAP
    }));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(earr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(entries_slot));

    // Rebox the handle block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// The i64/f64 `ms` payload slot of a `TYPE_DATE` block, 8-byte aligned.
fn date_ms_slot() -> MemArg {
    MemArg {
        offset: DATE_MS_OFFSET,
        align: 3,
        memory_index: 0,
    }
}

/// Emit `new Date()` / `new Date(ms)`, leaving the fresh `TYPE_DATE`
/// handle jsval on the stack. A [`DATE_BLOCK_SIZE`] block is bump-allocated; its
/// type byte is written and its f64 payload set to the millisecond timestamp —
/// either `ms` (a number jsval, whose raw i64 IS the f64 bit pattern, reinterpreted
/// here) or, when `ms` is `None`, the host wall clock (`__now`, already an f64).
pub(super) fn emit_date_new_node(
    ms: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // dst = __alloc(DATE_BLOCK_SIZE); write the TYPE_DATE byte.
    function.instruction(&Instruction::I32Const(DATE_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_DATE));
    function.instruction(&Instruction::I32Store8(type_byte));

    // ms slot = f64 timestamp. Push the store address, then the f64 value.
    function.instruction(&Instruction::LocalGet(dst));
    match ms {
        Some(node) => {
            emit_node(node, function, scratch, strings, helpers);
            function.instruction(&Instruction::F64ReinterpretI64);
        }
        None => {
            function.instruction(&Instruction::Call(helpers.now));
        }
    }
    function.instruction(&Instruction::F64Store(date_ms_slot()));

    // Rebox the block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit `Date.now()`: call the host `__now` clock (an f64 ms count) and
/// reinterpret it to a number jsval (a number's raw i64 IS its f64 bit pattern).
pub(super) fn emit_date_now_node(function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::Call(helpers.now));
    emit_box_f64(function, helpers);
}

/// Emit `d.getTime()`, leaving the receiver's millisecond timestamp as
/// a number jsval on the stack. The receiver is evaluated once and stashed; a
/// `TYPE_DATE` receiver's f64 payload is read back (reinterpreted to a number
/// jsval), and any other receiver yields the canonical number `NaN`.
pub(super) fn emit_date_get_time_node(
    recv: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let v = scratch + 17;
    emit_node(recv, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(v));

    // is_date = is_pointer(v) & (type_byte == TYPE_DATE), gated so the type-byte
    // read never runs for a non-pointer.
    emit_is_pointer(function, v);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, v);
    function.instruction(&Instruction::I32Const(TYPE_DATE));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::F64Load(date_ms_slot()));
    emit_box_f64(function, helpers);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::End);
}

/// Milliseconds per day — the divisor splitting an epoch-ms into a whole-day count
/// and a time-of-day remainder.
const MS_PER_DAY: i64 = 86_400_000;

/// The f64 exponent mask: a payload is non-finite (`NaN`/`Infinity`) iff every
/// exponent bit is set (`bits & MASK == MASK`), tested directly on the i64 bits.
const F64_EXP_MASK: i64 = 0x7FF0_0000_0000_0000;

/// Emit a `Date` field method call `recv.<op>(args)`, leaving the op's
/// result jsval on the stack. Mirrors [`emit_map_method_node`]: the receiver is
/// evaluated once into `recv_local`, then a runtime TYPE-BYTE dispatch splits the
/// paths — a `TYPE_DATE` block computes the field GUEST-side from its stored f64
/// epoch-ms ([`emit_date_field`]); any other receiver falls back to an ordinary
/// user method call `recv.<method_name>(args)`, so an object/class method of the
/// same name still works. When the module declares no functions the fallback is
/// dead and emits `undefined` instead of an invalid `call_indirect`.
pub(super) fn emit_date_method_node(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::DateMethod {
        op,
        recv,
        args,
        method_name,
        recv_local,
        fallback_obj_local,
    } = node
    else {
        unreachable!("emit_date_method_node is only called with a DateMethod node")
    };
    let recv_local = *recv_local;

    // Evaluate the receiver ONCE into a stable frame local.
    emit_node(recv, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(recv_local));

    // is_date = is_pointer(recv) && type_byte == TYPE_DATE. The pointer gate keeps
    // the type-byte load off a non-heap receiver.
    emit_is_pointer(function, recv_local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, recv_local);
    function.instruction(&Instruction::I32Const(TYPE_DATE));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // Either arm may wrap a throwing sub-call one frame deeper (mirrors MapMethod).
    enter_frame(helpers);
    emit_date_field(*op, recv_local, function, scratch, strings, helpers);
    function.instruction(&Instruction::Else);
    if helpers.fn_type == 0 {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    } else {
        let fallback = IrNode::MethodCallUser {
            obj: Box::new(IrNode::Local(recv_local)),
            key: Box::new(IrNode::Str(method_name.clone())),
            args: args.clone(),
            obj_local: *fallback_obj_local,
        };
        emit_method_call_user(&fallback, function, scratch, strings, helpers);
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Emit the `TYPE_DATE` compute path of [`emit_date_method_node`], leaving the
/// field's jsval on the stack. `valueOf()` returns the raw stored ms (a `NaN`
/// timestamp passes straight through); every other field first gates on a FINITE
/// timestamp — a `NaN`/`Infinity` date yields `NaN` (numeric fields) or the
/// `"Invalid Date"` string (`toISOString`). The finite path derives the field from
/// the f64 payload via [`emit_date_int_field`] / [`emit_date_iso`].
fn emit_date_field(
    op: DateOp,
    recv_local: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // valueOf() = getTime() = the raw stored ms (a number's i64 IS its f64 bits).
    if matches!(op, DateOp::ValueOf) {
        function.instruction(&Instruction::LocalGet(recv_local));
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::F64Load(date_ms_slot()));
        emit_box_f64(function, helpers);
        return;
    }
    // Stash the payload BITS (`D0`) and gate on finiteness.
    let bits = scratch + 24;
    function.instruction(&Instruction::LocalGet(recv_local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(date_ms_slot()));
    function.instruction(&Instruction::LocalSet(bits));
    function.instruction(&Instruction::LocalGet(bits));
    function.instruction(&Instruction::I64Const(F64_EXP_MASK));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(F64_EXP_MASK));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    if matches!(op, DateOp::ToIsoString) {
        emit_date_iso(bits, function, scratch, helpers);
    } else {
        emit_date_int_field(op, bits, function, scratch);
        // Integer field -> a number jsval (a number's i64 IS its f64 bit pattern).
        function.instruction(&Instruction::F64ConvertI64S);
        function.instruction(&Instruction::I64ReinterpretF64);
    }
    function.instruction(&Instruction::Else);
    if matches!(op, DateOp::ToIsoString) {
        let offset = strings.get(INVALID_DATE).copied().unwrap_or(HEAP_BASE);
        function.instruction(&Instruction::I64Const(HEAP_BITS | i64::from(offset)));
    } else {
        function.instruction(&Instruction::I64Const(QNAN_BITS));
    }
    function.instruction(&Instruction::End);
}

/// Compute a whole-day count into `days` and the time-of-day (ms) into `tod` from
/// the f64 timestamp BITS in `bits` (finite-guarded by the caller). Uses FLOOR
/// division so a pre-epoch (negative) timestamp splits correctly (`tod` stays in
/// `0..MS_PER_DAY`). `t0`/`t1` are extra i64 temps.
fn emit_days_tod(function: &mut Function, bits: u32, days: u32, tod: u32, t0: u32, t1: u32) {
    // total = floor(reinterpret(bits)) -> t0.
    function.instruction(&Instruction::LocalGet(bits));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Floor);
    function.instruction(&Instruction::I64TruncF64S);
    function.instruction(&Instruction::LocalSet(t0));
    // q0 = total / MS_PER_DAY -> days (truncated toward zero).
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::I64Const(MS_PER_DAY));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::LocalSet(days));
    // r0 = total - q0*MS_PER_DAY -> tod (same sign as total).
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::LocalGet(days));
    function.instruction(&Instruction::I64Const(MS_PER_DAY));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalSet(tod));
    // neg = (tod < 0) -> t1; days -= neg; tod += neg*MS_PER_DAY (floor adjustment).
    function.instruction(&Instruction::LocalGet(tod));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::I64LtS);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::LocalSet(t1));
    function.instruction(&Instruction::LocalGet(days));
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalSet(days));
    function.instruction(&Instruction::LocalGet(tod));
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(MS_PER_DAY));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalSet(tod));
}

/// Civil-from-days (Howard Hinnant): compute the UTC `year` (proleptic), `month`
/// (1..12), and `day` (1..31) for the whole-day count in `days`. `t0`/`t1`/`t2`
/// plus the three outputs are used as intermediate storage. Valid across the
/// pre-epoch range (the `era` numerator adjusts for negative day counts).
#[allow(clippy::too_many_arguments)]
fn emit_date_civil(
    function: &mut Function,
    days: u32,
    year: u32,
    month: u32,
    day: u32,
    t0: u32,
    t1: u32,
    t2: u32,
) {
    // z = days + 719468 -> t0.
    function.instruction(&Instruction::LocalGet(days));
    function.instruction(&Instruction::I64Const(719_468));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalSet(t0));
    // era = (z >= 0 ? z : z - 146096) / 146097 -> t1.
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::I64Const(146_096));
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::I64GeS);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::I64Const(146_097));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::LocalSet(t1));
    // doe = z - era*146097 -> t2.
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(146_097));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalSet(t2));
    // yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365 -> day (temp).
    function.instruction(&Instruction::LocalGet(t2));
    function.instruction(&Instruction::LocalGet(t2));
    function.instruction(&Instruction::I64Const(1460));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalGet(t2));
    function.instruction(&Instruction::I64Const(36_524));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalGet(t2));
    function.instruction(&Instruction::I64Const(146_096));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::I64Const(365));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::LocalSet(day));
    // year0 = yoe + era*400 -> year.
    function.instruction(&Instruction::LocalGet(day));
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(400));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalSet(year));
    // doy = doe - (365*yoe + yoe/4 - yoe/100) -> t0 (z free).
    function.instruction(&Instruction::LocalGet(t2));
    function.instruction(&Instruction::LocalGet(day));
    function.instruction(&Instruction::I64Const(365));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::LocalGet(day));
    function.instruction(&Instruction::I64Const(4));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalGet(day));
    function.instruction(&Instruction::I64Const(100));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalSet(t0));
    // mp = (5*doy + 2) / 153 -> t1 (era free).
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::I64Const(5));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Const(2));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::I64Const(153));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::LocalSet(t1));
    // day = doy - (153*mp + 2)/5 + 1.
    function.instruction(&Instruction::LocalGet(t0));
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(153));
    function.instruction(&Instruction::I64Mul);
    function.instruction(&Instruction::I64Const(2));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::I64Const(5));
    function.instruction(&Instruction::I64DivS);
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::I64Const(1));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalSet(day));
    // month = mp < 10 ? mp + 3 : mp - 9.
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(3));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(9));
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalGet(t1));
    function.instruction(&Instruction::I64Const(10));
    function.instruction(&Instruction::I64LtS);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(month));
    // year = month <= 2 ? year0 + 1 : year0.
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::I64Const(1));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::LocalGet(month));
    function.instruction(&Instruction::I64Const(2));
    function.instruction(&Instruction::I64LeS);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(year));
}

/// Emit the integer-valued `Date` getter for `op` (all UTC), leaving the raw i64
/// field on the stack (the caller boxes it into a number jsval). Uses the `Date`
/// scratch slots (`D0`..`D5` = `scratch+24..+29`) plus one low i64 temp; a `Date`
/// method takes no arguments, so no child sub-expression is live in these slots.
fn emit_date_int_field(op: DateOp, bits: u32, function: &mut Function, scratch: u32) {
    let days = scratch + 26;
    let tod = scratch + 27;
    emit_days_tod(function, bits, days, tod, scratch + 25, scratch + 28);
    match op {
        DateOp::GetHours => {
            function.instruction(&Instruction::LocalGet(tod));
            function.instruction(&Instruction::I64Const(3_600_000));
            function.instruction(&Instruction::I64DivS);
        }
        DateOp::GetMinutes => {
            function.instruction(&Instruction::LocalGet(tod));
            function.instruction(&Instruction::I64Const(60_000));
            function.instruction(&Instruction::I64DivS);
            function.instruction(&Instruction::I64Const(60));
            function.instruction(&Instruction::I64RemS);
        }
        DateOp::GetSeconds => {
            function.instruction(&Instruction::LocalGet(tod));
            function.instruction(&Instruction::I64Const(1000));
            function.instruction(&Instruction::I64DivS);
            function.instruction(&Instruction::I64Const(60));
            function.instruction(&Instruction::I64RemS);
        }
        DateOp::GetDay => {
            // ((days + 4) mod 7), Euclidean so a pre-epoch day count stays in 0..6.
            let x = scratch + 24;
            let r = scratch + 25;
            function.instruction(&Instruction::LocalGet(days));
            function.instruction(&Instruction::I64Const(4));
            function.instruction(&Instruction::I64Add);
            function.instruction(&Instruction::LocalSet(x));
            function.instruction(&Instruction::LocalGet(x));
            function.instruction(&Instruction::LocalGet(x));
            function.instruction(&Instruction::I64Const(7));
            function.instruction(&Instruction::I64DivS);
            function.instruction(&Instruction::I64Const(7));
            function.instruction(&Instruction::I64Mul);
            function.instruction(&Instruction::I64Sub);
            function.instruction(&Instruction::LocalSet(r));
            function.instruction(&Instruction::LocalGet(r));
            function.instruction(&Instruction::LocalGet(r));
            function.instruction(&Instruction::I64Const(0));
            function.instruction(&Instruction::I64LtS);
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(7));
            function.instruction(&Instruction::I64Mul);
            function.instruction(&Instruction::I64Add);
        }
        DateOp::GetFullYear | DateOp::GetMonth | DateOp::GetDate => {
            let year = scratch + 29;
            let month = scratch + 28;
            let day = scratch + 27;
            emit_date_civil(
                function,
                days,
                year,
                month,
                day,
                scratch + 24,
                scratch + 25,
                scratch,
            );
            match op {
                DateOp::GetFullYear => {
                    function.instruction(&Instruction::LocalGet(year));
                }
                DateOp::GetMonth => {
                    // getMonth() is 0-based.
                    function.instruction(&Instruction::LocalGet(month));
                    function.instruction(&Instruction::I64Const(1));
                    function.instruction(&Instruction::I64Sub);
                }
                _ => {
                    function.instruction(&Instruction::LocalGet(day));
                }
            }
        }
        DateOp::ValueOf | DateOp::ToIsoString => {
            unreachable!("valueOf / toISOString are handled by emit_date_field")
        }
    }
}

/// Emit `toISOString()`, leaving a fresh 24-byte heap string jsval
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`, UTC) on the stack. The date/time fields are derived
/// from the finite payload BITS in `bits`; a fixed-width buffer is bump-allocated
/// and each separator/digit written at its known payload offset. Years outside
/// `0..=9999` render with a truncated 4-digit field (a documented cut).
fn emit_date_iso(bits: u32, function: &mut Function, scratch: u32, helpers: Helpers<'_>) {
    let days = scratch + 26;
    let tod = scratch + 1;
    let year = scratch + 29;
    let month = scratch + 28;
    let day = scratch + 27;
    let tmp = scratch + 24;
    let bptr = scratch + 3;
    emit_days_tod(function, bits, days, tod, scratch + 25, scratch + 28);
    emit_date_civil(
        function,
        days,
        year,
        month,
        day,
        scratch + 24,
        scratch + 25,
        scratch,
    );

    // Allocate the block: HEADER + u32 len + 24 payload bytes; write type + len.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4 + ISO_STRING_LEN));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(bptr));
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I32Const(ISO_STRING_LEN));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));

    // Fixed separator bytes at their payload offsets.
    for (pos, ch) in [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (23, b'Z'),
    ] {
        emit_iso_byte(function, bptr, pos, ch);
    }
    // Date fields, then time fields (each computed just before it is written).
    emit_iso_digits(function, bptr, 0, year, 4);
    emit_iso_digits(function, bptr, 5, month, 2);
    emit_iso_digits(function, bptr, 8, day, 2);
    for (pos, div, rem, ndigits) in [
        (11, 3_600_000_i64, 0_i64, 2),
        (14, 60_000, 60, 2),
        (17, 1000, 60, 2),
        (20, 1, 1000, 3),
    ] {
        function.instruction(&Instruction::LocalGet(tod));
        function.instruction(&Instruction::I64Const(div));
        function.instruction(&Instruction::I64DivS);
        if rem != 0 {
            function.instruction(&Instruction::I64Const(rem));
            function.instruction(&Instruction::I64RemS);
        }
        function.instruction(&Instruction::LocalSet(tmp));
        emit_iso_digits(function, bptr, pos, tmp, ndigits);
    }

    // Rebox the block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Write one fixed ASCII byte `ch` at payload offset `pos` of the string block in
/// `bptr` (payload begins at `HEADER_SIZE + 4`).
fn emit_iso_byte(function: &mut Function, bptr: u32, pos: u64, ch: u8) {
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I32Const(i32::from(ch)));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: (HEADER_SIZE as u64) + 4 + pos,
        align: 0,
        memory_index: 0,
    }));
}

/// Write the low `ndigits` decimal digits of the i64 in `value` as zero-padded
/// ASCII at payload offset `pos` (most-significant first) of the string block in
/// `bptr`.
fn emit_iso_digits(function: &mut Function, bptr: u32, pos: u64, value: u32, ndigits: u32) {
    for i in 0..ndigits {
        let power = 10_i64.pow(ndigits - 1 - i);
        function.instruction(&Instruction::LocalGet(bptr));
        function.instruction(&Instruction::LocalGet(value));
        function.instruction(&Instruction::I64Const(power));
        function.instruction(&Instruction::I64DivS);
        function.instruction(&Instruction::I64Const(10));
        function.instruction(&Instruction::I64RemS);
        function.instruction(&Instruction::I64Const(i64::from(b'0')));
        function.instruction(&Instruction::I64Add);
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::I32Store8(MemArg {
            offset: (HEADER_SIZE as u64) + 4 + pos + u64::from(i),
            align: 0,
            memory_index: 0,
        }));
    }
}

/// Emit a `Map`/`Set` method call `recv.<op>(args)`, leaving the op's result
/// jsval on the stack. The receiver is evaluated once into `recv_local`, then a
/// runtime TYPE-BYTE dispatch splits the paths: a `TYPE_MAP`/`TYPE_SET` handle runs
/// the collection op through the `__map_*` helpers (`.set`/`.add` yield the handle;
/// `.get` the value/undefined; `.has`/`.delete` a boxed boolean); any other receiver
/// falls back to an ordinary user method call `recv.<method_name>(args)` — so an
/// object/class method of the same name still works. When the module declares no
/// functions (no funcref table exists, so no object could carry a method) the
/// fallback arm is dead and emits `undefined` instead of an invalid `call_indirect`.
pub(super) fn emit_map_method_node(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::MapMethod {
        op,
        recv,
        args,
        method_name,
        recv_local,
        fallback_obj_local,
    } = node
    else {
        unreachable!("emit_map_method_node is only called with a MapMethod node")
    };
    let recv_local = *recv_local;

    // Evaluate the receiver ONCE into a stable frame local.
    emit_node(recv, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(recv_local));

    // is_collection = is_pointer(recv) && (type_byte == TYPE_MAP || == TYPE_SET).
    // The pointer gate keeps the type-byte load off a non-heap receiver.
    emit_is_pointer(function, recv_local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, recv_local);
    function.instruction(&Instruction::I32Const(TYPE_MAP));
    function.instruction(&Instruction::I32Eq);
    emit_load_type_byte(function, recv_local);
    function.instruction(&Instruction::I32Const(TYPE_SET));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // Either arm may wrap a throwing sub-call, one frame deeper (mirrors Ternary).
    enter_frame(helpers);
    // MAP/SET op path: `handle` is the wrapped receiver base.
    emit_map_op(*op, recv_local, args, function, scratch, strings, helpers);
    function.instruction(&Instruction::Else);
    if helpers.fn_type == 0 {
        // No funcref table: an object method call is impossible, so the fallback is
        // dead code — emit a well-typed `undefined` to keep the module valid.
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    } else {
        // Fallback: an ordinary user method call `recv.<name>(args)` binding
        // `this = recv`, reusing the method-call machinery.
        let fallback = IrNode::MethodCallUser {
            obj: Box::new(IrNode::Local(recv_local)),
            key: Box::new(IrNode::Str(method_name.clone())),
            args: args.clone(),
            obj_local: *fallback_obj_local,
        };
        emit_method_call_user(&fallback, function, scratch, strings, helpers);
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Emit the `TYPE_MAP`/`TYPE_SET` collection-op path of [`emit_map_method_node`],
/// leaving the op's result jsval on the stack. `handle` holds the receiver jsval;
/// its wrapped base is the first argument to each `__map_*` helper. A missing key/
/// value argument is `undefined` (JS treats `map.set()` as `set(undefined,
/// undefined)`); `.set`/`.add` yield the receiver handle (chainable), `.has`/
/// `.delete` box their `1`/`0` result as a boolean jsval.
fn emit_map_op(
    op: MapOp,
    handle: u32,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // Push argument slot `i`, or `undefined` when the call supplied fewer.
    let push_arg = |function: &mut Function, i: usize| match args.get(i) {
        Some(arg) => emit_node(arg, function, scratch, strings, helpers),
        None => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    };
    match op {
        MapOp::Set => {
            function.instruction(&Instruction::LocalGet(handle));
            function.instruction(&Instruction::I32WrapI64);
            push_arg(function, 0);
            push_arg(function, 1);
            function.instruction(&Instruction::Call(helpers.map_set));
            function.instruction(&Instruction::LocalGet(handle));
        }
        MapOp::Add => {
            function.instruction(&Instruction::LocalGet(handle));
            function.instruction(&Instruction::I32WrapI64);
            push_arg(function, 0);
            // A Set stores its value slot as `undefined`.
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            function.instruction(&Instruction::Call(helpers.map_set));
            function.instruction(&Instruction::LocalGet(handle));
        }
        MapOp::Get => {
            function.instruction(&Instruction::LocalGet(handle));
            function.instruction(&Instruction::I32WrapI64);
            push_arg(function, 0);
            function.instruction(&Instruction::Call(helpers.map_get));
        }
        MapOp::Has | MapOp::Delete => {
            function.instruction(&Instruction::LocalGet(handle));
            function.instruction(&Instruction::I32WrapI64);
            push_arg(function, 0);
            let helper = if matches!(op, MapOp::Has) {
                helpers.map_has
            } else {
                helpers.map_delete
            };
            function.instruction(&Instruction::Call(helper));
            // Box the `1`/`0` i32 into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::I64Or);
        }
    }
}

/// Emit an object-rest binding `{...rest} = source`, leaving the fresh
/// object jsval on the stack. `source` (the hidden temp holding the initializer)
/// and `exclude` (an [`IrNode::Array`] of the destructured key strings) are each
/// lowered and unwrapped to their block base offsets, passed to the `__obj_rest`
/// runtime helper, and its returned base is reboxed as a heap jsval.
pub(super) fn emit_object_rest_node(
    source: &IrNode,
    exclude: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // obj_ptr = i32.wrap(source jsval); excl_ptr = i32.wrap(exclude array jsval).
    emit_node(source, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    emit_node(exclude, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.obj_rest));
    // Rebox the new object block's base offset as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Push i32 `1` when the number index in `s1` is in bounds for the heap
/// array/string in `s0` (`0 <= idx < len`), else `0`. The length is the u32
/// header at `+HEADER_SIZE`, shared by arrays and strings. `idx` is read via
/// SIGNED saturating truncation for the `>= 0` half so a negative index fails
/// (unsigned truncation would clamp `-1` to `0`, spuriously in bounds); the
/// `< len` half compares unsigned against the u32 length.
fn emit_index_in_bounds(function: &mut Function, s0: u32, s1: u32) {
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    // idx (signed) >= 0
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64S);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32GeS);
    // idx (unsigned) < len
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::I32And);
}

/// Emit the string-index char build for `s0[s1]`: `__str_index` on the wrapped
/// pointer and unsigned-truncated index, then rebox the returned base offset as
/// a heap jsval. Assumes `s1` is in bounds (the caller gates it).
fn emit_str_index_boxed(function: &mut Function, s0: u32, s1: u32, helpers: Helpers<'_>) {
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::Call(helpers.str_index));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit a computed-index expression `obj[index]`, dispatching at RUNTIME on
/// `obj`'s TYPE BYTE and leaving the resulting jsval on the stack. An
/// object (`TYPE_OBJECT`) scans its entries via `__obj_get`; an array
/// (`TYPE_ARRAY`) loads the element jsval at `ptr + HEADER_SIZE + 4 + 8*index`
/// (already boxed — not reboxed); anything else is the string path, which
/// allocates a fresh one-char heap string via `__str_index` (reboxed as a heap
/// jsval).
///
/// `obj` and `index` are each evaluated once into a scratch local so neither
/// path re-runs them; the branch reads the stashed bits. `obj` is a heap pointer
/// at runtime, so reading its type byte is safe.
///
/// The stash uses the DEDICATED HIGH scratch slots `+17`/`+18` rather
/// than `+0`/`+1`: the `index` sub-expression is emitted with base `scratch` and
/// reuses `+0..+13`, so a computed index (e.g. `a[i+1]`) would otherwise clobber
/// the already-stashed receiver before the load reads it back.
///
/// A receiver that may be `null`/`undefined` (see [`never_nullish`]) is guarded
/// by [`emit_nullish_guard`] once both operands are stashed; `checked = false`
/// skips the guard for a caller that already ran it on the same receiver.
pub(super) fn emit_index_node(
    obj: &IrNode,
    index: &IrNode,
    checked: bool,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let s0 = scratch + 17;
    let s1 = scratch + 18;
    // Evaluate and stash the operand and the index jsval into the dedicated high
    // slots (child emits below reuse `+0..+13` and so can't touch these).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s0));
    emit_node(index, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    if checked && !never_nullish(obj) {
        emit_nullish_guard(
            function,
            s0,
            &IrNode::Local(s1),
            false,
            scratch,
            strings,
            helpers,
        );
    }

    // A `Map`/`Set` handle intercepts property reads FIRST: the only bare
    // member read reaching here on a collection is `.size` (its `.get`/`.set`/… are
    // method calls, lowered separately), so any index on a `TYPE_MAP`/`TYPE_SET`
    // block yields its `size` header (a number jsval), never the string/object path.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_MAP));
    function.instruction(&Instruction::I32Eq);
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_SET));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // size = u32 at s0 + HEADER_SIZE, reboxed as a number jsval.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::Else);

    // Runtime type-byte dispatch (if/else chain so only the matching path runs):
    // object → key scan; array → element load; anything else → the string path.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // OBJECT: the index jsval is a heap-string key pointer; scan the entries via
    // `__obj_get`, whose i64 result (the matching value jsval or `undefined`) is
    // left on the stack as-is.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.obj_get));
    function.instruction(&Instruction::Else);
    // Branch on the array type byte; anything else is treated as a string.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // ARRAY: out-of-bounds (index >= len or negative) yields `undefined`; in range
    // loads the i64 element jsval at ptr + HEADER_SIZE + 4 + 8*i. The loaded value
    // is already a boxed jsval, so it is returned as-is.
    emit_index_in_bounds(function, s0, s1);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    // Branch on the string type byte; anything else (e.g. a function receiver) is
    // NOT a real string, so it keeps the prior unchecked `__str_index` path — an
    // out-of-range read there traps rather than silently mis-dispatching.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // STRING: out-of-bounds (index >= len or negative) yields `undefined`; in range
    // builds a fresh one-char string via `__str_index` (reboxed as a heap jsval).
    emit_index_in_bounds(function, s0, s1);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    emit_str_index_boxed(function, s0, s1, helpers);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    // Non-string, non-array, non-object receiver: preserve the prior behavior.
    emit_str_index_boxed(function, s0, s1, helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // Close the outer Map/Set `.size` dispatch.
    function.instruction(&Instruction::End);
}

/// Emit an [`IrNode::ObjectKey`] — the key at `index` of `obj` as a heap-string
/// jsval — dispatching at RUNTIME on `obj`'s TYPE BYTE. An object
/// (`TYPE_OBJECT`) loads the key jsval already stored at entry `index`
/// (`ptr + HEADER_SIZE + 4 + 16*index`, mirroring [`emit_obj_get`]'s key slot);
/// anything else (an array, or a string) coerces the numeric `index` to its
/// decimal string via the host `__convert` to-string op, so `for-in` over an
/// array/string yields `"0"`, `"1"`, .. keys.
///
/// `obj` and `index` are each stashed into the dedicated high READ scratch slots
/// `+17`/`+18` (like [`emit_index_node`]) so the runtime dispatch can read both
/// back without re-running them. `for-in` always passes plain locals here, so the
/// same-kind-nesting caveat those slots carry does not bite.
pub(super) fn emit_object_key_node(
    obj: &IrNode,
    index: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let s0 = scratch + 17;
    let s1 = scratch + 18;
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s0));
    emit_node(index, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));

    // Runtime type-byte dispatch: object → stored key string; else → String(index).
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // OBJECT: load the key jsval at ptr + HEADER_SIZE + 4 + 16*index. The stored
    // key is already a heap-string jsval, so it is returned as-is.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::Else);
    // ARRAY / STRING: the index IS the key; coerce it to its decimal string
    // (`String(index)`) through the host `__convert` to-string op.
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::End);
}

/// Emit a computed assignment `obj[index] = value`, storing `value` into
/// an EXISTING slot and leaving `value` on the stack as the expression's result.
/// The operation runtime-dispatches on `obj`'s TYPE BYTE, mirroring the
/// read side [`emit_index_node`]: an object (`TYPE_OBJECT`) treats `index` as a
/// heap-string key and calls `__obj_set` to store into the matching entry; an
/// array (`TYPE_ARRAY`) stores the element jsval at `ptr + HEADER_SIZE + 4 +
/// 8*index` when `index` is an integer in `[0, length)`. Objects and arrays are
/// fixed-shape, so every other store throws a `TypeError` through the
/// out-of-line `__prop_throw` without writing memory: an absent object key, a
/// non-index array key, or any other heap receiver raises the add-property
/// error; an array index at/past the length raises the grow error. The object
/// hit path costs one branch on `__obj_set`'s hit flag; the array hit path the
/// index/bounds compares.
///
/// `obj`, `index`, and `value` are each evaluated once (left-to-right) into
/// scratch i64 locals so neither branch re-runs them; both branches leave the
/// stashed `value` on the stack.
///
/// The stash uses the DEDICATED HIGH scratch slots `+14`/`+15`/`+16`
/// rather than `+0`/`+1`/`+2`: the `index` and `value` sub-expressions are
/// emitted with base `scratch` and reuse `+0..+13`, so a computed RHS (e.g.
/// `o.v = 3+4`, `this.x = a+b`) would otherwise clobber the already-stashed
/// receiver/key before the store reads them back. These are DISJOINT from the
/// read path's `+17`/`+18`, so a value child that is itself an index read (e.g.
/// `o.v = a[0]`) is safe as well.
///
/// Like the read side, a possibly-nullish receiver is guarded (after all three
/// operands are evaluated, matching JS `PutValue` order).
pub(super) fn emit_index_set_node(
    obj: &IrNode,
    index: &IrNode,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let s0 = scratch + 14;
    let s1 = scratch + 15;
    let sval = scratch + 16;
    // Evaluate and stash the operand, the index jsval, and the assigned value into
    // the dedicated high slots (child emits below reuse `+0..+13`, not these).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s0));
    emit_node(index, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(sval));
    if !never_nullish(obj) {
        emit_nullish_guard(
            function,
            s0,
            &IrNode::Local(s1),
            true,
            scratch,
            strings,
            helpers,
        );
    }

    // Runtime type-byte dispatch (if/else so only the matching path runs): object
    // → key scan store; anything else → the array element store. `obj` is a heap
    // pointer at runtime, so reading its type byte is safe. Both arms can unwind,
    // so the dispatch `if` is tracked as a control frame.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // OBJECT: the index jsval is a heap-string key pointer. When the program has
    // accessors, first resolve the key THROUGH the prototype chain: a write
    // that lands on an accessor property invokes its setter instead of storing a
    // data slot (a get-only accessor silently ignores the write). Otherwise the
    // plain own-slot `__obj_set` store, throwing if the key is absent.
    if helpers.has_accessors {
        emit_index_set_object_accessor(function, scratch, s0, s1, sval, helpers);
    } else {
        emit_obj_set_or_throw(function, s0, s1, sval, helpers);
    }
    function.instruction(&Instruction::Else);
    emit_index_set_array(function, s0, s1, sval, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // The assignment expression yields the stored value.
    function.instruction(&Instruction::LocalGet(sval));
}

/// Emit the own-slot `__obj_set(s0, s1, sval)` store and, when it reports a miss
/// (the key is absent), raise the add-property `TypeError`. The hit path is the
/// call plus one `i32.eqz`/`if` on its result flag.
fn emit_obj_set_or_throw(
    function: &mut Function,
    s0: u32,
    s1: u32,
    sval: u32,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(sval));
    function.instruction(&Instruction::Call(helpers.obj_set));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_prop_throw_call(function, s1, false, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Emit the non-object arm of [`emit_index_set_node`]. A `TYPE_ARRAY` receiver
/// with a number key `k` that is a non-negative integer (`k >= 0` and
/// `k - trunc(k) == 0`, false for `NaN`/`±Infinity`) stores the element at
/// `ptr + HEADER_SIZE + 4 + 8*k` when `k < length`, else raises the grow
/// `TypeError`. Any other key, and any receiver that is neither an object nor an
/// array, raises the add-property `TypeError` naming `String(key)`. The key
/// reinterpretation is pure, so a non-number key reads no memory. The caller
/// tracks the enclosing dispatch frame.
fn emit_index_set_array(
    function: &mut Function,
    s0: u32,
    s1: u32,
    sval: u32,
    helpers: Helpers<'_>,
) {
    let key_f64 = |function: &mut Function| {
        function.instruction(&Instruction::LocalGet(s1));
        function.instruction(&Instruction::F64ReinterpretI64);
    };
    // is_array && is_number(key) && key >= 0 && key - trunc(key) == 0
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    emit_is_number(function, s1);
    function.instruction(&Instruction::I32And);
    key_f64(function);
    function.instruction(&Instruction::F64Const(0.0.into()));
    function.instruction(&Instruction::F64Ge);
    function.instruction(&Instruction::I32And);
    key_f64(function);
    key_f64(function);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Sub);
    function.instruction(&Instruction::F64Const(0.0.into()));
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // key < length (the u32 header at `ptr + HEADER_SIZE`).
    key_f64(function);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // In bounds: store the element jsval at ptr + HEADER_SIZE + 4 + 8*i (the
    // header offset folded into the store's MemArg). Mirrors `emit_index_node`'s
    // array arm.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    key_f64(function);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(sval));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::Else);
    emit_prop_throw_call(function, s1, true, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    emit_prop_throw_call(function, s1, false, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Call `__prop_throw(key, grow)` on the key stashed in local `key` and unwind
/// like `throw`. The caller tracks every enclosing control frame.
fn emit_prop_throw_call(function: &mut Function, key: u32, grow: bool, helpers: Helpers<'_>) {
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32Const(i32::from(grow)));
    function.instruction(&Instruction::Call(helpers.prop_throw));
    emit_throw_return(function, helpers);
}

/// Emit the accessor-aware OBJECT store for `obj[key] = value` (the object arm
/// of [`emit_index_set_node`], only on the `has_accessors` path). `s0`/`s1`/`sval`
/// are the stashed receiver / key / value slots. The key is resolved THROUGH the
/// prototype chain via `__obj_get`: if it lands on a `TYPE_ACCESSOR` box with a
/// setter, the setter is invoked as `setter(value)` with `this` = the receiver and
/// its result discarded (a get-only box silently ignores the write, matching
/// non-strict JS); otherwise the plain `__obj_set` own-slot store runs, throwing
/// the add-property `TypeError` if the key is absent.
///
/// Every `if` opened here is tracked with [`enter_frame`]/[`leave_frame`] (the
/// caller tracks its `TYPE_OBJECT` dispatch `if`), so a setter or miss that
/// throws inside a `try` computes the right handler-relative `br` depth.
fn emit_index_set_object_accessor(
    function: &mut Function,
    scratch: u32,
    s0: u32,
    s1: u32,
    sval: u32,
    helpers: Helpers<'_>,
) {
    // `+17`/`+18` (the index READ slots) are free during a write, so they hold the
    // resolved value and the setter jsval; `+7` is the standard indirect-call base.
    let resolved = scratch + 17;
    let setter = scratch + 18;
    let base = scratch + 7;

    // resolved = __obj_get(s0, s1) — walks the prototype chain, so an accessor
    // declared on the class prototype is found from a bare instance.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.obj_get));
    function.instruction(&Instruction::LocalSet(resolved));

    emit_is_accessor(function, resolved);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // setter = the setter jsval stored in the box.
    function.instruction(&Instruction::LocalGet(resolved));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: ACCESSOR_SET_OFFSET,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalSet(setter));

    emit_is_function(function, setter);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // Invoke the setter: the assigned value as arg0 (a setter takes exactly one),
    // the rest of the uniform K slots padded `undefined`; `this` = the receiver.
    // (K is 0 only when no function is parametered — then no setter exists and this
    // branch is unreachable at runtime, so pushing no arg keeps the wasm valid.)
    if helpers.max_args >= 1 {
        function.instruction(&Instruction::LocalGet(sval));
        for _ in 1..helpers.max_args {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    }
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
    function.instruction(&Instruction::LocalGet(setter));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 3,
        memory_index: 0,
    }));
    emit_argc(1, function);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    emit_propagate_if_pending(function, helpers);
    // Discard the setter's return value (an assignment yields the assigned value,
    // handled by the caller's trailing `LocalGet(sval)`).
    function.instruction(&Instruction::Drop);
    leave_frame(helpers);
    function.instruction(&Instruction::End); // is_function (no else: get-only box no-ops)
    function.instruction(&Instruction::Else);
    // Not an accessor: the ordinary own-slot store (throws if the key is absent).
    emit_obj_set_or_throw(function, s0, s1, sval, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End); // is_accessor
}

/// Emit a compound element/member assignment `obj[index] op= rhs`.
///
/// `obj` and `index` are evaluated exactly once into the stable temp locals
/// `obj_local`/`idx_local` (not scratch, which the read below clobbers), so no
/// sub-expression side effect runs twice. The new value — current member read
/// through those temps, combined with `rhs` via `op` (reusing the [`Binary`]
/// overload, so `+=` on a string member concatenates) — is staged in `new_local`,
/// then the store is delegated to [`emit_index_set_node`] over the three stable
/// temps. Delegating with a `Local` `value` keeps the store's scratch intact.
pub(super) fn emit_index_op_set_node(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::IndexOpSet {
        obj,
        index,
        op,
        rhs,
        obj_local,
        idx_local,
        new_local,
        kind,
    } = node
    else {
        unreachable!("emit_index_op_set_node is only called with an IndexOpSet node")
    };
    let (op, obj_local, idx_local, new_local) = (*op, *obj_local, *idx_local, *new_local);
    // Evaluate the base and key ONCE into stable temps.
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(obj_local));
    emit_node(index, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(idx_local));
    // The current-value read throws on a nullish base before `rhs` runs; the read
    // below reuses the same receiver, so it skips its own guard.
    if !never_nullish(obj) {
        emit_nullish_guard(
            function,
            obj_local,
            &IrNode::Local(idx_local),
            false,
            scratch,
            strings,
            helpers,
        );
    }

    // new = current_member op rhs. The member read reuses the runtime type-byte
    // dispatch, reads its operands from the stable temps, and is staged in
    // `new_local` (before `rhs` runs) so the combine reads it back as a `Local`.
    emit_index_node(
        &IrNode::Local(obj_local),
        &IrNode::Local(idx_local),
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    if helpers.has_accessors {
        emit_accessor_get(function, scratch, helpers);
    }
    function.instruction(&Instruction::LocalSet(new_local));
    let new_value = IrNode::Binary {
        op,
        lhs: Box::new(IrNode::Local(new_local)),
        rhs: rhs.clone(),
    };
    // A member `++`/`--` converts the current value with ToNumber first.
    if *kind != OpSetKind::Compound {
        let old_num = IrNode::ConvertCall {
            op: ConvertOp::ToNumber,
            arg: Box::new(IrNode::Local(new_local)),
        };
        emit_node(&old_num, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(new_local));
    }
    // A postfix update yields the OLD numeric value: store `old op rhs`, then drop
    // the stored value in favour of `new_local`.
    if *kind == OpSetKind::PostfixUpdate {
        emit_index_set_node(
            &IrNode::Local(obj_local),
            &IrNode::Local(idx_local),
            &new_value,
            function,
            scratch,
            strings,
            helpers,
        );
        function.instruction(&Instruction::Drop);
        function.instruction(&Instruction::LocalGet(new_local));
        return;
    }
    emit_node(&new_value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(new_local));

    // Store back into the same member, reusing the plain-store dispatch. All three
    // operands are `Local` reads, so evaluating them does not disturb scratch.
    emit_index_set_node(
        &IrNode::Local(obj_local),
        &IrNode::Local(idx_local),
        &IrNode::Local(new_local),
        function,
        scratch,
        strings,
        helpers,
    );
}

/// Whether `node` provably never evaluates to `null`/`undefined`, so a member
/// access on it needs no nullish-receiver guard: literals and freshly built
/// heap values (object/array literals, `new` instances, collections, function
/// values) are always real receivers.
fn never_nullish(node: &IrNode) -> bool {
    matches!(
        node,
        IrNode::Num(_)
            | IrNode::Str(_)
            | IrNode::Bool(_)
            | IrNode::Object(_)
            | IrNode::Array(_)
            | IrNode::ArraySpread(_)
            | IrNode::New { .. }
            | IrNode::MapNew { .. }
            | IrNode::DateNew { .. }
            | IrNode::FuncValue { .. }
    )
}

/// Emit an [`IrNode::NullishCheck`]: evaluate `value`, throw the read
/// `TypeError` naming `key` if it is `null`/`undefined`, else leave it on the
/// stack. A statically non-nullish value is emitted bare. The value is stashed in
/// the general i64 scratch slot (`+0`), which only the guard's throw path could
/// disturb.
pub(super) fn emit_nullish_check_node(
    value: &IrNode,
    key: &str,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(value, function, scratch, strings, helpers);
    if never_nullish(value) {
        return;
    }
    function.instruction(&Instruction::LocalSet(scratch));
    let key = IrNode::Str(key.to_owned());
    emit_nullish_guard(function, scratch, &key, false, scratch, strings, helpers);
    function.instruction(&Instruction::LocalGet(scratch));
}

/// Emit the nullish-receiver guard for a member access on the receiver stashed
/// in local `recv`: if it is `null` or `undefined` — one NaN-box tag compare,
/// since the two share every bit above bit 48 except the low tag bit — call the
/// out-of-line `__nullish_throw(recv, key, set)` and unwind like `throw`. `key`
/// is evaluated only on that throw path. The not-nullish path falls through with
/// the stack untouched.
pub(super) fn emit_nullish_guard(
    function: &mut Function,
    recv: u32,
    key: &IrNode,
    set: bool,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(recv));
    function.instruction(&Instruction::I64Const(49));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(NULL_BITS >> 49));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // The `if` opens a control frame around the throw transfer.
    enter_frame(helpers);
    function.instruction(&Instruction::LocalGet(recv));
    emit_node(key, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32Const(i32::from(set)));
    function.instruction(&Instruction::Call(helpers.nullish_throw));
    emit_throw_return(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// `__nullish_throw(recv: i64, key: i64, set: i32)`: build the branded
/// `TypeError` for a member read (`set == 0`) or write (`set == 1`) on a
/// `null`/`undefined` `recv`, stash it in `$exc_value`, set the pending flag and
/// clear any routed completion. The message is spliced from the
/// [`NULLISH_ERROR_STRS`] fragments plus `String(key)`:
/// `Cannot read properties of null (reading 'x')`, with `.stack` prefixed
/// `TypeError: `.
pub(super) fn emit_nullish_throw(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    // Params occupy locals 0..3; the shared `TypeError`-helper locals follow.
    let (recv, key, set) = (0, 1, 2);
    let scratch = 3;
    let msg = scratch + 24;
    let mut function = Function::new(type_error_helper_locals());
    let at = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE);
    let select = |function: &mut Function, if_set: &str, if_read: &str| {
        function.instruction(&Instruction::I32Const(at(if_set)));
        function.instruction(&Instruction::I32Const(at(if_read)));
        function.instruction(&Instruction::LocalGet(set));
        function.instruction(&Instruction::Select);
    };
    let concat = |function: &mut Function| {
        function.instruction(&Instruction::Call(helpers.concat));
    };

    // msg = prefix + (null|undefined) + mid + String(key) + "')"
    select(&mut function, NULLISH_SET_PREFIX, NULLISH_READ_PREFIX);
    function.instruction(&Instruction::I32Const(at("null")));
    function.instruction(&Instruction::I32Const(at("undefined")));
    function.instruction(&Instruction::LocalGet(recv));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::Select);
    concat(&mut function);
    select(&mut function, NULLISH_SET_MID, NULLISH_READ_MID);
    concat(&mut function);
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::I32WrapI64);
    concat(&mut function);
    function.instruction(&Instruction::I32Const(at(NULLISH_SUFFIX)));
    concat(&mut function);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(msg));

    emit_raise_type_error(&mut function, msg, scratch, strings, helpers);
    function.instruction(&Instruction::End);
    function
}

/// The locals of a `TypeError`-building helper after its params: the full
/// `emit_main` scratch layout (so `emit_node` can build the error object as it
/// would in `main`), then the message and stack string jsvals at `scratch + 24`
/// and `scratch + 25`.
fn type_error_helper_locals() -> Vec<(u32, ValType)> {
    vec![
        (3, ValType::I64),
        (1, ValType::I32),
        (3, ValType::I64),
        (1, ValType::I32),
        (5, ValType::I32),
        (1, ValType::I64),
        (7, ValType::I64),
        (1, ValType::I32),
        (2, ValType::I64),
        (2, ValType::I64),
    ]
}

/// Raise a branded `TypeError` whose message string jsval is in local `msg`:
/// build `stack = "TypeError: " + msg` into `msg + 1`, the error object, stash it
/// in `$exc_value`, set the pending flag and clear any routed completion.
fn emit_raise_type_error(
    function: &mut Function,
    msg: u32,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let stack = msg + 1;
    let at = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE);
    // stack = "TypeError: " + msg
    function.instruction(&Instruction::I32Const(at(TYPE_ERROR_STACK_PREFIX)));
    function.instruction(&Instruction::LocalGet(msg));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(stack));

    let error = IrNode::Object(vec![
        ("name".to_owned(), IrNode::Str("TypeError".to_owned())),
        ("message".to_owned(), IrNode::Local(msg)),
        ("stack".to_owned(), IrNode::Local(stack)),
        (
            abi::ERROR_KIND_KEY.to_owned(),
            IrNode::Num(abi::ERROR_KIND_TYPE),
        ),
    ]);
    emit_node(&error, function, scratch, strings, helpers);
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
}

/// `__prop_throw(key: i64, grow: i32)`: build the branded `TypeError` for a
/// member store the fixed-shape receiver cannot take, stash it in `$exc_value`,
/// set the pending flag and clear any routed completion. `grow == 0` is an absent
/// property — `Cannot add property '<String(key)>': dynamic properties are not
/// supported yet` from the [`PROP_ERROR_STRS`] fragments; `grow == 1` is an
/// array index store at/past the length — `Cannot grow array by index
/// assignment yet`.
pub(super) fn emit_prop_throw(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let (key, grow) = (0, 1);
    let scratch = 2;
    let msg = scratch + 24;
    let mut function = Function::new(type_error_helper_locals());
    let at = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE);

    // msg = grow ? ARRAY_GROW_MSG : prefix + String(key) + suffix
    function.instruction(&Instruction::LocalGet(grow));
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::I32Const(at(ARRAY_GROW_MSG)));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(at(PROP_ADD_PREFIX)));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::I32Const(at(PROP_ADD_SUFFIX)));
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(msg));

    emit_raise_type_error(&mut function, msg, scratch, strings, helpers);
    function.instruction(&Instruction::End);
    function
}
