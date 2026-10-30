#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit an object literal ([`IrNode::Object`]), leaving the object jsval on the
/// stack. One `__alloc` reserves the 16-byte `TYPE_OBJECT` prefix (with its
/// `class` byte) and, right after it, a `TYPE_PROPS` table of `cap =
/// max(entries, reserve)` entries (`{}` with no reserve gets `props = 0` and no
/// table). `proto` (or `null`) is written into the `proto` slot.
///
/// When every key is a distinct compile-time string, each entry is laid down
/// directly — the interned key constant with its attributes as one `i64`, then
/// the value — and the table header is a constant (`count = live = n`, its
/// `tflags` precomputed). Otherwise (a computed/numeric key or a duplicate
/// key) each entry is defined in order through `__define`, which converts and
/// interns a computed key and overwrites a duplicate in place. Mirrors
/// [`emit_array_node`]'s alloc→write→rebox shape, keeping `dst` on the operand
/// stack so nested objects/arrays in the key and value expressions compose.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // one straight-line literal layout
pub(super) fn emit_object_node(
    entries: &[(PropKey, IrNode)],
    proto: Option<&IrNode>,
    class: u8,
    reserve: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The i32 scratch local (declared just past the three i64 scratch locals) is
    // used only transiently, to duplicate `dst` before each write.
    let dst = scratch + 3;
    let mem = |offset: u64| MemArg {
        offset,
        align: 3,
        memory_index: 0,
    };
    let n = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let cap = n.max(reserve);
    let table = abi::OBJ_PREFIX_SIZE.cast_signed();
    let size = table
        + if cap == 0 {
            0
        } else {
            16 + 16 * cap.cast_signed()
        };
    let mut static_keys: Vec<&str> = Vec::with_capacity(entries.len());
    for (key, _) in entries {
        if let PropKey::Static(k) | PropKey::Hidden(k) | PropKey::Fixed(k) = key {
            static_keys.push(k);
        }
    }
    let direct = static_keys.len() == entries.len()
        && static_keys
            .iter()
            .enumerate()
            .all(|(i, k)| !static_keys[..i].contains(k));

    // dst = __alloc(size); header word (TYPE_OBJECT) and `props` (dst + 16, or 0).
    function.instruction(&Instruction::I32Const(size));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    let header = i64::from(TYPE_OBJECT) | i64::from(class) << 24;
    if cap == 0 {
        function.instruction(&Instruction::I64Const(header));
    } else {
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(table));
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::I64ExtendI32U);
        function.instruction(&Instruction::I64Const(32));
        function.instruction(&Instruction::I64Shl);
        function.instruction(&Instruction::I64Const(header));
        function.instruction(&Instruction::I64Or);
    }
    function.instruction(&Instruction::I64Store(mem(0)));
    // proto slot.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    match proto {
        Some(proto) => emit_node(proto, function, scratch, strings, helpers),
        None => {
            function.instruction(&Instruction::I64Const(intrinsic_bits(
                Intrinsic::ObjectPrototype,
                helpers,
            )));
        }
    }
    function.instruction(&Instruction::I64Store(mem(OBJ_PROTO_OFFSET)));
    if cap == 0 {
        emit_object_rebox(function);
        return;
    }

    // Table header: [kind=12][pad][tflags][pad][count] then [cap][live].
    let (count, tflags) = if direct {
        let mut tflags = 0u8;
        for (key, value) in entries {
            if attrs_of(key, value) & abi::ATTR_ENUMERABLE == 0 {
                tflags |= abi::PROPS_HAS_NON_ENUM;
            }
            if let PropKey::Static(k) | PropKey::Hidden(k) | PropKey::Fixed(k) = key
                && is_array_index(k)
            {
                tflags |= abi::PROPS_HAS_INDEX_KEYS;
            }
        }
        (n, tflags)
    } else {
        (0, 0)
    };
    let word = |lo: u32, hi: u32| (i64::from(hi) << 32) | i64::from(lo);
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64Const(word(
        u32::from(abi::TYPE_PROPS) | u32::from(tflags) << 16,
        count,
    )));
    function.instruction(&Instruction::I64Store(mem(u64::from(abi::OBJ_PREFIX_SIZE))));
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64Const(word(cap, count)));
    function.instruction(&Instruction::I64Store(mem(
        u64::from(abi::OBJ_PREFIX_SIZE) + 8
    )));

    for (k, (key, value)) in entries.iter().enumerate() {
        let attrs = attrs_of(key, value);
        if direct {
            let (PropKey::Static(name) | PropKey::Hidden(name) | PropKey::Fixed(name)) = key else {
                continue;
            };
            let entry = u64::from(abi::OBJ_PREFIX_SIZE) + 16 + 16 * k as u64;
            let interned = static_key(strings, name).cast_unsigned();
            function.instruction(&Instruction::LocalTee(dst));
            function.instruction(&Instruction::LocalGet(dst));
            function.instruction(&Instruction::I64Const(word(interned, attrs)));
            function.instruction(&Instruction::I64Store(mem(entry)));
            function.instruction(&Instruction::LocalTee(dst));
            function.instruction(&Instruction::LocalGet(dst));
            emit_node(value, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Store(mem(entry + 8)));
        } else {
            // __define(dst, `ToPropertyKey`(key), value, attrs), keeping dst below.
            function.instruction(&Instruction::LocalTee(dst));
            function.instruction(&Instruction::LocalGet(dst));
            emit_define_entry(key, value, function, scratch, strings, helpers);
        }
    }
    emit_object_rebox(function);
}

/// The `attrs` an entry `(key, value)` is defined with: an accessor value is
/// an `ACCESSOR` property, a [`PropKey::Fixed`] key is non-writable,
/// non-enumerable, and non-configurable, a [`PropKey::Hidden`] or
/// [`PropKey::HiddenComputed`] key is non-enumerable, and everything else is
/// writable and configurable.
fn attrs_of(key: &PropKey, value: &IrNode) -> u32 {
    let enumerable = if matches!(
        key,
        PropKey::Hidden(_) | PropKey::Fixed(_) | PropKey::HiddenComputed(_)
    ) {
        0
    } else {
        abi::ATTR_ENUMERABLE
    };
    if matches!(key, PropKey::Fixed(_)) {
        0
    } else if matches!(value, IrNode::MakeAccessor { .. }) {
        abi::ATTR_ACCESSOR | abi::ATTR_CONFIGURABLE | enumerable
    } else {
        abi::ATTR_WRITABLE | abi::ATTR_CONFIGURABLE | enumerable
    }
}

/// With the target block base already on the stack, push the entry's interned
/// key (a constant, or `ToPropertyKey` of a computed key), its value, and its
/// [`attrs_of`], and call `__define`. An accessor half (one of its getter /
/// setter `undefined`, see `push_accessor_half`) is a partial descriptor
/// instead, applied with `__define_own`: it keeps the other half of an
/// accessor already defined under the same key (`get [k]() {}` then
/// `set [k](v) {}`).
fn emit_define_entry(
    key: &PropKey,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match key {
        PropKey::Static(name) | PropKey::Hidden(name) | PropKey::Fixed(name) => {
            function.instruction(&Instruction::I32Const(static_key(strings, name)));
        }
        PropKey::Computed(expr) | PropKey::HiddenComputed(expr) => {
            emit_node(expr, function, scratch, strings, helpers);
            function.instruction(&Instruction::I32Const(1));
            function.instruction(&Instruction::Call(helpers.to_key));
        }
    }
    if let IrNode::MakeAccessor { getter, setter } = value {
        let half = match (getter.as_ref(), setter.as_ref()) {
            (IrNode::Undefined, _) => DESC_HAS_SET,
            (_, IrNode::Undefined) => DESC_HAS_GET,
            _ => 0,
        };
        if half != 0 {
            let attrs = attrs_of(key, value) & (abi::ATTR_ENUMERABLE | abi::ATTR_CONFIGURABLE);
            let desc = half | DESC_HAS_ENUMERABLE | DESC_HAS_CONFIGURABLE | attrs.cast_signed();
            function.instruction(&Instruction::I32Const(desc));
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            emit_node(getter, function, scratch, strings, helpers);
            emit_node(setter, function, scratch, strings, helpers);
            function.instruction(&Instruction::Call(helpers.define_own()));
            function.instruction(&Instruction::Drop);
            return;
        }
    }
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32Const(attrs_of(key, value).cast_signed()));
    function.instruction(&Instruction::Call(helpers.define));
}

/// Emit an [`IrNode::DefineProps`]: evaluate `obj` into `local`, define each
/// entry on it in order (entry values may read `local`), and leave `obj` on
/// the stack. An enumerable entry (a class field) is `CreateDataPropertyOrThrow`
/// on an object user code may have frozen or made non-extensible, so a
/// rejected `__define` propagates; the non-enumerable entries (methods, the
/// class `prototype`) target objects no user code has seen yet.
pub(super) fn emit_define_props_node(
    obj: &IrNode,
    local: u32,
    entries: &[(PropKey, IrNode)],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(local));
    for (key, value) in entries {
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::I32WrapI64);
        emit_define_entry(key, value, function, scratch, strings, helpers);
        if matches!(key, PropKey::Static(_) | PropKey::Computed(_)) {
            emit_propagate_if_pending(function, helpers);
        }
    }
    function.instruction(&Instruction::LocalGet(local));
}

/// Rebox the object block base still on the stack as a heap jsval.
fn emit_object_rebox(function: &mut Function) {
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
/// cannot corrupt the accessor block base. A `MakeAccessor` only ever appears as an
/// object-literal or class-prototype entry value, so the two closure builds are
/// the only nested emits and this stack discipline suffices.
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

/// Emit an `Object.keys/values/entries(arg)` collection built-in (or the
/// `for-in` key snapshot), leaving the fresh array's heap jsval on the stack:
/// `arg` is evaluated once and handed to `__own_keys` in the matching mode (or
/// to `__for_in_keys`); a `null`/`undefined` `arg` of `Object.*` throws (the
/// `ToObject` `TypeError`). `Object.values/entries` read through `__get`, so a
/// getter that throws is propagated.
pub(super) fn emit_object_builtin_node(
    op: ObjectBuiltinOp,
    arg: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(arg, function, scratch, strings, helpers);
    if op != ObjectBuiltinOp::ForInKeys {
        // `ToObject(arg)`: `null`/`undefined` is a TypeError.
        function.instruction(&Instruction::LocalSet(scratch));
        emit_is_nullish(function, scratch);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        function.instruction(&Instruction::LocalGet(scratch));
        function.instruction(&Instruction::I64Const(
            HEAP_BITS | i64::from(static_key(strings, "")),
        ));
        function.instruction(&Instruction::I32Const(PROP_THROW_TO_OBJECT));
        function.instruction(&Instruction::Call(helpers.prop_throw));
        emit_throw_return(function, helpers);
        leave_frame(helpers);
        function.instruction(&Instruction::End);
        function.instruction(&Instruction::LocalGet(scratch));
    }
    let mode = match op {
        ObjectBuiltinOp::Keys => OWN_KEYS,
        ObjectBuiltinOp::Values => OWN_VALUES,
        ObjectBuiltinOp::Entries => OWN_ENTRIES,
        ObjectBuiltinOp::ForInKeys => {
            function.instruction(&Instruction::Call(helpers.for_in_keys));
            return;
        }
    };
    function.instruction(&Instruction::I32Const(mode));
    function.instruction(&Instruction::Call(helpers.own_keys));
    if helpers.has_accessors && mode != OWN_KEYS {
        emit_propagate_if_pending(function, helpers);
    }
}

/// Emit a `key in obj` membership test, leaving a `true`/`false` jsval on
/// the stack. `key` is evaluated and converted with `ToPropertyKey` (a lookup: a
/// never-interned key is absent), then `obj` is evaluated and `__has` searches
/// it and its `proto` chain. Its `1`/`0` result is boxed
/// into a boolean jsval (`FALSE_BITS | bit`).
pub(super) fn emit_object_has_node(
    key: &IrNode,
    obj: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_key_lookup(key, function, scratch, strings, helpers);
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.has));
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an `x instanceof RHS` identity test, leaving a `true`/`false`
/// jsval on the stack. The operand `value` is evaluated first, then the
/// right-hand side. The runtime-TYPE-BYTE builtins test the operand's heap kind;
/// the prototype kinds gate the operand with [`emit_is_object`] (a primitive is
/// never an instance) and walk its `proto` chain for
/// the target prototype: the static error prototype ([`InstanceOfKind::Proto`])
/// or, for [`InstanceOfKind::Ctor`] (`OrdinaryHasInstance`), `Get(C,
/// "prototype")` of the callable-guarded right-hand side `C`, which must be an
/// object. An object `C` whose chain holds a `@@hasInstance` (`__symbol_get`)
/// throws the interim `TypeError` first (Y4 honours it).
#[allow(clippy::too_many_lines)] // straight-line per-kind emit, not branching depth
pub(super) fn emit_instanceof_node(
    value: &IrNode,
    kind: &InstanceOfKind,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(value, function, scratch, strings, helpers);
    // The runtime-TYPE-BYTE builtins test the receiver's heap kind directly:
    // `Array`/`Function`/`Date` match their own byte; `Object` matches ANY
    // object (a primitive string or symbol is not one). The operand is
    // stashed in a dedicated high slot.
    let v = scratch + 17;
    let (rhs, ctor) = match kind {
        InstanceOfKind::Array
        | InstanceOfKind::Function
        | InstanceOfKind::Date
        | InstanceOfKind::Object => {
            function.instruction(&Instruction::LocalSet(v));
            let type_byte = match kind {
                InstanceOfKind::Array => Some(TYPE_ARRAY),
                InstanceOfKind::Function => Some(TYPE_FUNCTION),
                InstanceOfKind::Date => Some(TYPE_DATE),
                _ => None,
            };
            return emit_type_byte_instanceof(function, v, type_byte);
        }
        InstanceOfKind::Proto(proto) => (proto, false),
        InstanceOfKind::Ctor(ctor) => (ctor, true),
    };

    // `cur` walks the receiver's `proto` chain; `tgt` holds the right-hand side
    // and then the target prototype. Both are dedicated high slots (`+19`/`+20`)
    // the `prototype` read below (`+17`/`+18`) does not touch. The operand rides
    // the operand stack while the right-hand side evaluates.
    let cur = scratch + 19;
    let tgt = scratch + 20;
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(tgt));
    function.instruction(&Instruction::LocalSet(cur));
    if ctor {
        // A `@@hasInstance` on an object `C`'s chain is not honoured before Y4:
        // the interim `TypeError` (design/symbol-strings.md §9.4).
        emit_is_object(function, tgt);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        emit_hook_guard(
            function,
            tgt,
            WellKnown::HasInstance,
            scratch,
            strings,
            helpers,
        );
        if helpers.has_accessors {
            emit_propagate_if_pending(function, helpers);
        }
        leave_frame(helpers);
        function.instruction(&Instruction::End);
        // `IsCallable(C)`, checked before the receiver (a primitive operand still
        // throws for a non-callable right-hand side).
        emit_callable_guard(
            function,
            INSTANCEOF_NOT_CALLABLE,
            tgt,
            scratch,
            strings,
            helpers,
        );
        // OrdinaryHasInstance step 2: a bound `C` tests its target instead.
        emit_unwrap_bound(function, tgt);
    }

    emit_is_object(function, cur);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    if ctor {
        // P = Get(C, "prototype"); a non-object P throws.
        emit_get_call(
            function,
            tgt,
            &IrNode::Str("prototype".to_owned()),
            scratch + 18,
            strings,
            helpers,
        );
        if helpers.has_accessors {
            emit_propagate_if_pending(function, helpers);
        }
        function.instruction(&Instruction::LocalSet(tgt));
        emit_is_object(function, tgt);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        emit_throw_not_a_function(
            function,
            INSTANCEOF_NON_OBJECT_PROTOTYPE,
            scratch,
            strings,
            helpers,
        );
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }
    // Walk the receiver's `proto` chain for pointer identity with the target
    // prototype: `true` iff some link is the SAME block. A chain ends at `null`.
    let proto_slot = MemArg {
        offset: OBJ_PROTO_OFFSET,
        align: 3,
        memory_index: 0,
    };
    // cur = recv.proto.
    function.instruction(&Instruction::LocalGet(cur));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(proto_slot));
    function.instruction(&Instruction::LocalSet(cur));
    // Loop: 1 iff `cur` reaches `tgt`, 0 iff the chain runs out.
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
    // cur = cur.proto — the next link up the chain.
    function.instruction(&Instruction::LocalGet(cur));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(proto_slot));
    function.instruction(&Instruction::LocalSet(cur));
    function.instruction(&Instruction::Br(0)); // continue the loop
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::I32Const(0)); // unreachable fall-through
    function.instruction(&Instruction::End); // block -> i32 on the stack
    // Box the i32 `1`/`0` into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
    leave_frame(helpers);
    function.instruction(&Instruction::Else);
    // A receiver without a `proto` slot is never an instance.
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::End);
}

/// Replace the function in i64 local `f` by its bound target while it is a
/// bound function (the first element of its `env` array, DESIGN.md §5.5).
fn emit_unwrap_bound(function: &mut Function, f: u32) {
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(f));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(i32::from(abi::FUNC_FLAG_BOUND)));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(f));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32WrapI64);
    emit_array_elems(function);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::LocalSet(f));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Emit a runtime-TYPE-BYTE `instanceof` test for the operand already stashed in
/// local `v`, leaving a TRUE/FALSE jsval on the stack. `true` iff `v` is a heap
/// pointer whose type byte equals `type_byte` — or, for `None` (the
/// `instanceof Object` case), iff `v` is an object ([`emit_is_object`]: any heap
/// kind but a primitive string or symbol). The type-byte read is gated behind
/// [`emit_is_pointer`] so a non-pointer operand (whose wrapped low bits would be
/// a bogus offset) is simply never an instance.
fn emit_type_byte_instanceof(function: &mut Function, v: u32, type_byte: Option<i32>) {
    let Some(type_byte) = type_byte else {
        emit_is_object(function, v);
        function.instruction(&Instruction::I64ExtendI32U);
        function.instruction(&Instruction::I64Const(FALSE_BITS));
        function.instruction(&Instruction::I64Or);
        return;
    };
    emit_is_pointer(function, v);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    emit_load_type_byte(function, v);
    function.instruction(&Instruction::I32Const(type_byte));
    function.instruction(&Instruction::I32Eq);
    // Box the i32 `1`/`0` into a TRUE/FALSE jsval (`FALSE_BITS | bit`).
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::Else);
    // A non-pointer operand is never an instance.
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::End);
}

/// Emit a `delete obj[key]` property removal, leaving a `true` jsval on the
/// stack. `obj` is evaluated, then the key is converted with `ToPropertyKey` (a
/// lookup: a never-interned key is absent) and `__delete` tombstones the own
/// property; a non-configurable one throws.
pub(super) fn emit_object_delete_node(
    obj: &IrNode,
    key: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(obj, function, scratch, strings, helpers);
    emit_key_lookup(key, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.delete));
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::I64Const(TRUE_BITS));
}

/// Emit a `JSON.stringify(arg)` call, leaving the serialized heap-string
/// jsval on the stack (or the `undefined` jsval when `arg` is not
/// serializable, e.g. `undefined` or a function value): `__json_rep` of
/// `arg` as the property `""` of no holder, with no replacer or property
/// list and a fresh, empty cycle stack. A throw (from a getter, a `toJSON`,
/// or a cycle) propagates.
pub(super) fn emit_json_stringify_node(
    arg: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Const(
        HEAP_BITS | i64::from(static_key(strings, "")),
    ));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.new_array()));
    function.instruction(&Instruction::Call(helpers.json_rep()));
    emit_propagate_if_pending(function, helpers);
}

/// Emit a `new Map()` / `new Set()` construction, leaving the fresh handle
/// jsval on the stack. Two blocks are allocated: an empty backing array
/// (`__new_array(0)`, grown in place) and the handle itself
/// (`TYPE_MAP`/`TYPE_SET`), whose `size` header starts at 0 and whose `entries`
/// slot holds the backing array's heap jsval.
pub(super) fn emit_map_new_node(
    set: bool,
    function: &mut Function,
    scratch: u32,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let earr = scratch + 7;
    let size_slot = MemArg {
        offset: MAP_SIZE_OFFSET,
        align: 2,
        memory_index: 0,
    };
    let entries_slot = MemArg {
        offset: MAP_ENTRIES_OFFSET,
        align: 3,
        memory_index: 0,
    };

    // Empty backing array: __new_array(0).
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.new_array()));
    function.instruction(&Instruction::LocalSet(earr));

    // Handle: __alloc(MAP_BLOCK_SIZE); object prefix; size = 0;
    // entries = rebox(earr).
    function.instruction(&Instruction::I32Const(MAP_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));
    emit_object_prefix(
        function,
        dst,
        if set { TYPE_SET } else { TYPE_MAP },
        intrinsic_bits(
            Intrinsic::Prototype(if set {
                BuiltinCtor::Set
            } else {
                BuiltinCtor::Map
            }),
            helpers,
        ),
    );
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(size_slot));
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

/// Emit `new Date()` / `new Date(value)`, leaving the fresh `TYPE_DATE`
/// handle jsval on the stack. The time value is computed first — `value`'s
/// own time value when it is a Date (`thisTimeValue`, not `ToPrimitive`),
/// else `ToNumber(value)`; when `arg` is `None`, the host wall clock (`__now`,
/// already an f64) — and passed through [`emit_time_clip`], the single point
/// every stored Date time value goes through. Only then is a
/// [`DATE_BLOCK_SIZE`] block bump-allocated, its object-family prefix
/// written and its f64 payload set.
pub(super) fn emit_date_new_node(
    arg: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let v = scratch;
    let dst = scratch + 3;
    let time = scratch + 24;

    match arg {
        Some(node) => {
            emit_node(node, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(v));
            // A Date argument contributes its stored time value.
            emit_is_pointer(function, v);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            emit_load_type_byte(function, v);
            function.instruction(&Instruction::I32Const(TYPE_DATE));
            function.instruction(&Instruction::I32Eq);
            function.instruction(&Instruction::Else);
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::End);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::LocalGet(v));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::I64Load(date_ms_slot()));
            function.instruction(&Instruction::LocalSet(v));
            function.instruction(&Instruction::End);
            // A number (a Date's time value included) passes through.
            emit_coerce_number(function, v, helpers);
            function.instruction(&Instruction::LocalGet(v));
            function.instruction(&Instruction::F64ReinterpretI64);
        }
        None => {
            function.instruction(&Instruction::Call(helpers.now));
        }
    }
    emit_time_clip(function, time);

    // dst = __alloc(DATE_BLOCK_SIZE); write the TYPE_DATE object prefix.
    function.instruction(&Instruction::I32Const(DATE_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));
    emit_object_prefix(
        function,
        dst,
        TYPE_DATE,
        intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Date), helpers),
    );

    // ms slot = the clipped f64 time value.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(time));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Store(date_ms_slot()));

    // Rebox the block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// The largest magnitude of a valid Date time value (ES2024 §21.4.1.1).
const MAX_TIME_VALUE: f64 = 8.64e15;

/// `TimeClip` (ES2024 §21.4.1.31) of the f64 on the stack, leaving its bits in
/// i64 `out`: `NaN` unless finite with `|t| <= 8.64e15`, else `t` truncated
/// toward zero with `-0` normalized to `+0`.
fn emit_time_clip(function: &mut Function, out: u32) {
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(out));
    // trunc(t) + 0 turns a -0 into +0.
    function.instruction(&Instruction::LocalGet(out));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Add);
    function.instruction(&Instruction::F64Const(Ieee64::from(f64::NAN)));
    // |t| <= MAX_TIME_VALUE is false for NaN and the infinities.
    function.instruction(&Instruction::LocalGet(out));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Abs);
    function.instruction(&Instruction::F64Const(Ieee64::from(MAX_TIME_VALUE)));
    function.instruction(&Instruction::F64Le);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(out));
}

/// Emit `Date.now()`: the host `__now` clock (an f64 ms count) through
/// [`emit_time_clip`] (a time value is integral), boxed as a number jsval.
pub(super) fn emit_date_now_node(function: &mut Function, scratch: u32, helpers: Helpers<'_>) {
    let time = scratch + 24;
    function.instruction(&Instruction::Call(helpers.now));
    emit_time_clip(function, time);
    function.instruction(&Instruction::LocalGet(time));
    function.instruction(&Instruction::F64ReinterpretI64);
    emit_box_f64(function, helpers);
}

/// Milliseconds per day — the divisor splitting an epoch-ms into a whole-day count
/// and a time-of-day remainder.
const MS_PER_DAY: i64 = 86_400_000;

/// The f64 exponent mask: a payload is non-finite (`NaN`/`Infinity`) iff every
/// exponent bit is set (`bits & MASK == MASK`), tested directly on the i64 bits.
const F64_EXP_MASK: i64 = 0x7FF0_0000_0000_0000;

/// The `RangeError` `toISOString()` throws for an invalid time value
/// (`[message, stack]`).
pub(super) const INVALID_TIME_VALUE: [&str; 2] =
    ["Invalid time value", "RangeError: Invalid time value"];

/// The [`INVALID_TIME_VALUE`] error object.
fn invalid_time_value() -> IrNode {
    IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(INVALID_TIME_VALUE[0].to_owned())),
        IrNode::Str(INVALID_TIME_VALUE[1].to_owned()),
    )
}

/// Emit the field `op` of the `TYPE_DATE` block in `recv_local`
/// ([`IrNode::DateField`]), leaving the field's jsval on the stack. `valueOf()` returns the raw stored ms (a `NaN`
/// timestamp passes straight through); every other field first gates on a FINITE
/// timestamp — an invalid (`NaN`) date yields `NaN` (numeric fields), the
/// `"Invalid Date"` string (`toString`) or throws [`invalid_time_value`]
/// (`toISOString`). The finite path derives the field from the f64 payload via
/// [`emit_date_int_field`] / [`emit_date_iso`] / [`emit_date_string`].
pub(super) fn emit_date_field(
    op: DateOp,
    recv_local: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // valueOf() = getTime() = the raw stored ms (a number's i64 IS its f64 bits).
    if matches!(op, DateOp::ValueOf | DateOp::GetTime) {
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
    enter_frame(helpers);
    if matches!(op, DateOp::ToIsoString) {
        emit_date_iso(bits, function, scratch, helpers);
    } else if matches!(op, DateOp::ToString) {
        emit_date_string(bits, function, scratch, strings, helpers);
    } else {
        emit_date_int_field(op, bits, function, scratch);
        // Integer field -> a number jsval (a number's i64 IS its f64 bit pattern).
        function.instruction(&Instruction::F64ConvertI64S);
        function.instruction(&Instruction::I64ReinterpretF64);
    }
    function.instruction(&Instruction::Else);
    match op {
        DateOp::ToIsoString => {
            emit_throw_error(&invalid_time_value(), function, scratch, strings, helpers);
        }
        DateOp::ToString => {
            let offset = strings.get(INVALID_DATE).copied().unwrap_or(HEAP_BASE);
            function.instruction(&Instruction::I64Const(HEAP_BITS | i64::from(offset)));
        }
        _ => {
            function.instruction(&Instruction::I64Const(QNAN_BITS));
        }
    }
    leave_frame(helpers);
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
        DateOp::GetTime | DateOp::ValueOf | DateOp::ToIsoString | DateOp::ToString => {
            unreachable!("valueOf / toISOString are handled by emit_date_field")
        }
    }
}

/// Emit `toISOString()`, leaving a fresh heap string jsval
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`, UTC) on the stack. The date/time fields are
/// derived from the finite payload BITS in `bits`; a buffer is bump-allocated
/// and each separator/digit written at its known payload offset. A year
/// outside `0..=9999` is the expanded `±YYYYYY` form, three bytes longer: every
/// field after the year is written through `at` = `bptr + 3` instead of `bptr`.
fn emit_date_iso(bits: u32, function: &mut Function, scratch: u32, helpers: Helpers<'_>) {
    let days = scratch + 26;
    let tod = scratch + 1;
    let year = scratch + 29;
    let month = scratch + 28;
    let day = scratch + 27;
    let tmp = scratch + 24;
    let abs_year = scratch + 25;
    let bptr = scratch + 3;
    let at = scratch + 8;
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

    // The expanded-year shift (0 or 3) -> `at`, as a byte count for now.
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::I64Const(9999));
    function.instruction(&Instruction::I64GtU);
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::LocalSet(at));

    // Allocate the block: HEADER + u32 len + payload; write type + len.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4 + ISO_STRING_LEN));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I32Add);
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
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));

    emit_iso_year(function, bptr, at, year, abs_year);
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(at));

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
        emit_iso_byte(function, at, pos, ch);
    }
    // Date fields, then time fields (each computed just before it is written).
    emit_iso_digits(function, at, 5, month, 2);
    emit_iso_digits(function, at, 8, day, 2);
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
        emit_iso_digits(function, at, pos, tmp, ndigits);
    }

    // Rebox the block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Write the `toISOString()` year field of the i64 `year` at the start of the
/// string block in `bptr`: four digits, or (when the shift in i32 `at` is
/// nonzero) a sign and six digits of `|year|`, kept in i64 `abs_year`.
fn emit_iso_year(function: &mut Function, bptr: u32, at: u32, year: u32, abs_year: u32) {
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(bptr));
    function.instruction(&Instruction::I32Const(i32::from(b'+')));
    function.instruction(&Instruction::I32Const(i32::from(b'-')));
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::I64GeS);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: (HEADER_SIZE as u64) + 4,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::I64Sub);
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::LocalGet(year));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::I64LtS);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(abs_year));
    emit_iso_digits(function, bptr, 1, abs_year, 6);
    function.instruction(&Instruction::Else);
    emit_iso_digits(function, bptr, 0, year, 4);
    function.instruction(&Instruction::End);
}

/// The three-letter weekday names, Sunday first (`Date.prototype.toString`).
pub(super) const DATE_WEEKDAYS: &str = "SunMonTueWedThuFriSat";

/// The three-letter month names, January first.
pub(super) const DATE_MONTHS: &str = "JanFebMarAprMayJunJulAugSepOctNovDec";

/// The `Date.prototype.toString` text after the year, its time digits
/// overwritten per date (jsc dates are UTC).
pub(super) const DATE_TAIL: &str = " 00:00:00 GMT+0000 (Coordinated Universal Time)";

/// Emit the `Date.prototype.toString` text of the finite timestamp BITS in
/// `bits` (`Www Mmm DD YYYY HH:MM:SS GMT+0000 (Coordinated Universal Time)`,
/// the year zero-padded to four digits and signed when negative) as a fresh
/// heap string, leaving its jsval on the stack. Uses the `Date` scratch
/// slots of [`emit_date_iso`] plus `+0` and the i32 slots `+8..+12`.
#[allow(clippy::too_many_lines)]
fn emit_date_string(
    bits: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    use Instruction as I;
    let (days, tod, year, month, day) = (
        scratch + 26,
        scratch + 1,
        scratch + 29,
        scratch + 28,
        scratch + 27,
    );
    let (weekday, abs_year, t) = (scratch + 24, scratch + 25, scratch);
    let (bptr, digits, sign, at) = (scratch + 3, scratch + 8, scratch + 9, scratch + 10);
    let payload = u64::from((HEADER_SIZE + 4).cast_unsigned());
    let tail = i32::try_from(DATE_TAIL.len()).unwrap_or(0);
    let wasm = |function: &mut Function, ops: &[Instruction<'_>]| {
        for op in ops {
            function.instruction(op);
        }
    };
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
    // weekday = (days mod 7 + 11) mod 7 (day 0 was a Thursday); |year|, its
    // sign and its digit count (at least four).
    wasm(
        function,
        &[
            I::LocalGet(days),
            I::I64Const(7),
            I::I64RemS,
            I::I64Const(11),
            I::I64Add,
            I::I64Const(7),
            I::I64RemS,
            I::LocalSet(weekday),
            I::I64Const(0),
            I::LocalGet(year),
            I::I64Sub,
            I::LocalGet(year),
            I::LocalGet(year),
            I::I64Const(0),
            I::I64LtS,
            I::LocalTee(sign),
            I::Select,
            I::LocalTee(abs_year),
            I::I64Const(10_000),
            I::I64DivU,
            I::LocalSet(t),
            I::I32Const(4),
            I::LocalSet(digits),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(t),
            I::I64Eqz,
            I::BrIf(1),
            I::LocalGet(digits),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(digits),
            I::LocalGet(t),
            I::I64Const(10),
            I::I64DivU,
            I::LocalSet(t),
            I::Br(0),
            I::End,
            I::End,
        ],
    );
    // The block: header + len + 58 fixed bytes + sign + digits.
    let fixed = 11 + tail;
    wasm(
        function,
        &[
            I::I32Const(HEADER_SIZE + 4 + fixed),
            I::LocalGet(sign),
            I::I32Add,
            I::LocalGet(digits),
            I::I32Add,
            I::Call(helpers.alloc),
            I::LocalTee(bptr),
            I::I32Const(TYPE_STRING),
            I::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::LocalGet(bptr),
            I::I32Const(fixed),
            I::LocalGet(sign),
            I::I32Add,
            I::LocalGet(digits),
            I::I32Add,
            I::I32Store(MemArg {
                offset: HEADER_SIZE as u64,
                align: 2,
                memory_index: 0,
            }),
        ],
    );
    // Weekday and month names, copied from their static tables.
    let table = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE) + HEADER_SIZE + 4;
    for (pos, names, index) in [(0, DATE_WEEKDAYS, weekday), (4, DATE_MONTHS, month)] {
        wasm(
            function,
            &[
                I::LocalGet(bptr),
                I::I32Const(HEADER_SIZE + 4 + pos),
                I::I32Add,
                I::I32Const(table(names)),
                I::LocalGet(index),
            ],
        );
        if pos == 4 {
            // `month` is 1-based.
            wasm(function, &[I::I64Const(1), I::I64Sub]);
        }
        wasm(
            function,
            &[
                I::I32WrapI64,
                I::I32Const(3),
                I::I32Mul,
                I::I32Add,
                I::I32Const(3),
                I::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                },
            ],
        );
    }
    for pos in [3, 7, 10] {
        emit_iso_byte(function, bptr, pos, b' ');
    }
    emit_iso_digits(function, bptr, 8, day, 2);
    // The sign, then the year digits right to left from `at`.
    wasm(
        function,
        &[
            I::LocalGet(bptr),
            I::I32Const(i32::from(b'-')),
            I::I32Store8(MemArg {
                offset: payload + 11,
                align: 0,
                memory_index: 0,
            }),
            I::LocalGet(bptr),
            I::LocalGet(sign),
            I::I32Add,
            I::LocalGet(digits),
            I::I32Add,
            I::LocalSet(at),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(digits),
            I::I32Eqz,
            I::BrIf(1),
            I::LocalGet(at),
            I::LocalGet(abs_year),
            I::I64Const(10),
            I::I64RemU,
            I::I64Const(i64::from(b'0')),
            I::I64Add,
            I::I32WrapI64,
            I::I32Store8(MemArg {
                offset: payload + 10,
                align: 0,
                memory_index: 0,
            }),
            I::LocalGet(abs_year),
            I::I64Const(10),
            I::I64DivU,
            I::LocalSet(abs_year),
            I::LocalGet(at),
            I::I32Const(1),
            I::I32Sub,
            I::LocalSet(at),
            I::LocalGet(digits),
            I::I32Const(1),
            I::I32Sub,
            I::LocalSet(digits),
            I::Br(0),
            I::End,
            I::End,
        ],
    );
    // The tail fills the last bytes; `at` = its offset from the payload.
    wasm(
        function,
        &[
            I::LocalGet(bptr),
            I::I32Load(MemArg {
                offset: HEADER_SIZE as u64,
                align: 2,
                memory_index: 0,
            }),
            I::I32Const(tail),
            I::I32Sub,
            I::LocalGet(bptr),
            I::I32Add,
            I::LocalTee(at),
            I::I32Const(HEADER_SIZE + 4),
            I::I32Add,
            I::I32Const(table(DATE_TAIL)),
            I::I32Const(tail),
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
        ],
    );
    for (pos, div, rem) in [(1, 3_600_000_i64, 0_i64), (4, 60_000, 60), (7, 1000, 60)] {
        wasm(function, &[I::LocalGet(tod), I::I64Const(div), I::I64DivS]);
        if rem != 0 {
            wasm(function, &[I::I64Const(rem), I::I64RemS]);
        }
        wasm(function, &[I::LocalSet(t)]);
        emit_iso_digits(function, at, pos, t, 2);
    }
    wasm(
        function,
        &[
            I::LocalGet(bptr),
            I::I64ExtendI32U,
            I::I64Const(HEAP_BITS),
            I::I64Or,
        ],
    );
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

/// Emit [`IrNode::CollectionOp`], leaving the op's result jsval on the stack.
/// `handle` holds the receiver jsval; its wrapped base is the first argument to
/// each `__map_*` helper. A missing key/value argument is `undefined` (JS
/// treats `map.set()` as `set(undefined, undefined)`); `.set`/`.add` yield the
/// receiver handle (chainable), `.has`/`.delete` box their `1`/`0` result as a
/// boolean jsval, `.clear` tombstones every entry and yields `undefined`.
pub(super) fn emit_map_op(
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
        MapOp::Clear => {
            emit_map_clear(handle, function, scratch);
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    }
}

/// `.clear()` on the handle in i64 `handle`: `size = 0`, and every entry
/// tombstoned in place (`HOLE` key, `undefined` value;
/// design/symbol-strings.md §5.5). The backing keeps its length, so a live
/// iterator continues at its index and sees the entries added after the
/// `clear`. Uses the i32 scratch locals `scratch + 8` / `+ 9`.
fn emit_map_clear(handle: u32, function: &mut Function, scratch: u32) {
    let (at, end) = (scratch + 8, scratch + 9);
    let slot = |offset| MemArg {
        offset,
        align: 3,
        memory_index: 0,
    };
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: MAP_SIZE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    // at = the first element slot; end = at + 8 * len.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(slot(MAP_ENTRIES_OFFSET)));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(end));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: ARRAY_ELEMS_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(at));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::LocalGet(end));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: ARRAY_LEN_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(end));
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::LocalGet(end));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Store(slot(0)));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Store(slot(8)));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(at));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Emit an object-rest binding `{...rest} = source`, leaving the fresh
/// object jsval on the stack: `source` (the hidden temp holding the
/// initializer) and `exclude` (an [`IrNode::Array`] of the destructured key
/// strings) are evaluated and handed to `__rest`.
pub(super) fn emit_object_rest_node(
    source: &IrNode,
    exclude: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(source, function, scratch, strings, helpers);
    emit_node(exclude, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.rest));
}

/// Push i32 `1` when the key jsval in `s1` is an integer index in bounds for the
/// heap array/string in `s0` (`0 <= idx < len`, `trunc(idx) == idx`), else `0`.
/// The length is the u32 at `+len_offset` ([`ARRAY_LEN_OFFSET`] for an array,
/// [`HEADER_SIZE`] for a string).
/// The compares run on `f64`, where every non-number jsval (a boxed tag) and the
/// canonical NaN read as NaN and so fail all three — a string key or `NaN`
/// is never mistaken for index `0`.
fn emit_index_in_bounds(function: &mut Function, s0: u32, s1: u32, len_offset: u64) {
    let key_f64 = |function: &mut Function| {
        function.instruction(&Instruction::LocalGet(s1));
        function.instruction(&Instruction::F64ReinterpretI64);
    };
    // idx >= 0
    key_f64(function);
    function.instruction(&Instruction::F64Const(0.0.into()));
    function.instruction(&Instruction::F64Ge);
    // idx < len
    key_f64(function);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: len_offset,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::I32And);
    // trunc(idx) == idx
    key_f64(function);
    function.instruction(&Instruction::F64Trunc);
    key_f64(function);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);
}

/// Push i32 `1` when the key jsval in `s1` is an integer index in bounds for
/// the heap array in `s0` ([`emit_index_in_bounds`]) and the array is not
/// `SPARSE` (whose elements past its store are not in it), else `0`.
fn emit_dense_index_in_bounds(function: &mut Function, s0: u32, s1: u32) {
    emit_index_in_bounds(function, s0, s1, ARRAY_LEN_OFFSET);
    emit_sparse_test(function, s0);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::I32And);
}

/// Push i32 nonzero when the heap array jsval in `s0` is `SPARSE`, else `0`.
fn emit_sparse_test(function: &mut Function, s0: u32) {
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(ARRAY_SPARSE));
    function.instruction(&Instruction::I32And);
}

/// Emit the string-key half of an array/string index read `s0[s1]`, leaving an
/// `i32` on the stack: `1` when `s1` is the string `"length"` (the caller yields
/// the length header), else `0`. Before returning `0`, a CANONICAL numeric
/// string key (`String(ToNumber(k)) === k`, e.g. `"1"` but not `"01"` or `""`)
/// is rewritten in place to its number, so the ordinary index path serves
/// `"abc"["1"]`; any other string key stays a pointer, which
/// [`emit_index_in_bounds`] rejects. A non-string key is left untouched. `tmp`
/// is a free i64 scratch local.
fn emit_index_string_key(
    function: &mut Function,
    s1: u32,
    tmp: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_is_string(function, s1);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Const(
        strings.get(INDEX_LENGTH_KEY).copied().unwrap_or(HEAP_BASE),
    ));
    function.instruction(&Instruction::Call(helpers.str_eq));
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::Else);
    // tmp = ToNumber(key); if String(tmp) === key then key = tmp.
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_NUMBER));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::LocalTee(tmp));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.str_eq));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push the u32 field at byte `offset` of the block whose jsval is in `s0`,
/// reboxed as a number jsval.
pub(super) fn emit_u32_field_boxed(function: &mut Function, s0: u32, offset: u64) {
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
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

/// Emit a member read `obj[index]` (`obj.k` is `obj["k"]`), leaving the
/// resulting jsval on the stack.
///
/// `obj` and `index` are each evaluated once into a scratch local. A static
/// string key goes straight to `__get` with its interned key constant
/// (`__get` resolves a string's or array's `length`/index keys itself, and a
/// primitive receiver's inherited ones through its prototype). A computed in-range integer key on an array loads the element (a hole
/// falls through to `__get`, which reads it through the prototype chain); a computed `"length"` or canonical numeric index on a
/// string reads it inline. Any other computed read converts the key with
/// `ToPropertyKey` (a lookup, so a never-interned key reads `undefined`) and
/// calls `__get` (which routes an array's canonical index strings to its
/// elements and its other keys to its own table).
///
/// Both operands are evaluated on the operand stack before being stashed in
/// the DEDICATED HIGH scratch slots `+17`/`+18`, so a computed index that is
/// itself a member read (`o[ks[i]]`) cannot clobber the receiver.
///
/// A receiver that may be `null`/`undefined` (see [`never_nullish`]) is guarded
/// by [`emit_nullish_guard`] once both operands are stashed; `checked = false`
/// skips the guard for a caller that already ran it on the same receiver. When
/// the program has accessors, a getter may have thrown inside `__get`, so the
/// read is followed by the pending-exception check.
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
    // Evaluate the operand and the index jsval (the operand rides the operand
    // stack, so a nested read in the index cannot clobber it), then stash both
    // into the dedicated high slots.
    emit_node(obj, function, scratch, strings, helpers);
    emit_node(index, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));
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

    // A static key is one `__get` (which resolves a primitive receiver
    // through its prototype); a computed key takes the inline array/string
    // reads first.
    match index {
        IrNode::Str(_) => emit_get_call(function, s0, index, s1, strings, helpers),
        _ => emit_index_dynamic(function, scratch, s0, s1, strings, helpers),
    }
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
}

/// Emit [`IrNode::SizeRead`], leaving `obj.size` on the stack: the `size`
/// header of a `Map`/`Set` handle passing its guard, else the ordinary member
/// read (the prototype getter, or an own/user `size`).
pub(super) fn emit_size_read(
    sealed: bool,
    obj: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let recv = scratch + 17;
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(recv));
    emit_proto_kind_test(
        function,
        recv,
        ProtoKind::Collection,
        sealed,
        scratch + 3,
        helpers,
    );
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    emit_u32_field_boxed(function, recv, MAP_SIZE_OFFSET);
    function.instruction(&Instruction::Else);
    enter_frame(helpers);
    let key = IrNode::Str("size".to_owned());
    emit_index_node(
        &IrNode::Local(recv),
        &key,
        true,
        function,
        scratch,
        strings,
        helpers,
    );
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Push `__get(s0, key)` for the receiver in `s0` and the property key `index`
/// (its jsval stashed in `s1`; a computed key is looked up, never inserted).
fn emit_get_call(
    function: &mut Function,
    s0: u32,
    index: &IrNode,
    s1: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(s0));
    emit_prop_key(function, index, s1, false, strings, helpers);
    function.instruction(&Instruction::Call(helpers.get));
}

/// Emit the computed-key half of [`emit_index_node`] for the receiver in `s0`
/// and the key jsval in `s1`: an array loads the element of an in-range
/// integer key, a string reads `length` or builds the in-range one-char
/// string, each leaving the read block early; every other key or receiver (a
/// primitive included) falls through to the one `__get`.
fn emit_index_dynamic(
    function: &mut Function,
    scratch: u32,
    s0: u32,
    s1: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::Block(BlockType::Result(ValType::I64)));
    emit_is_pointer(function, s0);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // ARRAY: an in-range integer key loads the i64 element jsval at
    // elems + 8 + 8*i (`__sparse_get` on a SPARSE array) into `scratch`; a
    // hole is not own, so it falls through to the `__get` that reads it
    // through the prototype chain.
    emit_index_in_bounds(function, s0, s1, ARRAY_LEN_OFFSET);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_sparse_test(function, s0);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::Call(helpers.sparse_get()));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::I32TruncSatF64U);
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalTee(scratch));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::Br(4));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // STRING: `"length"` reads the header and an in-range index (a number or
    // a canonical numeric string) builds a fresh one-char string via
    // `__str_index` (reboxed as a heap jsval).
    emit_index_string_key(function, s1, scratch, strings, helpers);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_u32_field_boxed(function, s0, HEADER_SIZE as u64);
    function.instruction(&Instruction::Br(4));
    function.instruction(&Instruction::End);
    emit_index_in_bounds(function, s0, s1, HEADER_SIZE as u64);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_str_index_boxed(function, s0, s1, helpers);
    function.instruction(&Instruction::Br(4));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // Every other key or receiver: the generic property read.
    emit_get_dynamic(function, s0, s1, helpers);
    function.instruction(&Instruction::End);
}

/// Push `__get(s0, ToPropertyKey(s1))` for the receiver in `s0` and the key
/// jsval in `s1` (a lookup: a never-interned key reads `undefined`).
fn emit_get_dynamic(function: &mut Function, s0: u32, s1: u32, helpers: Helpers<'_>) {
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.to_key));
    function.instruction(&Instruction::Call(helpers.get));
}

/// Emit a member assignment `obj[index] = value`, leaving `value` on the
/// stack as the expression's result.
///
/// A primitive receiver (number/boolean, or a heap string) throws the
/// strict-mode create-property `TypeError` before any heap access. An array
/// stores an in-range integer index inline and hands any other key to
/// `__array_set` (growth by index, canonical index strings, `length`, named
/// properties). Every other receiver — and any receiver under a static key
/// that is neither an array index nor `length` — goes through `__set`
/// (`OrdinarySet`: an accessor's setter, a read-only or non-extensible
/// `TypeError`, else an own data property, added when absent) with the key
/// converted by `ToPropertyKey` and interned. Both helpers may leave an
/// exception pending, so they are followed by the pending-exception check.
///
/// `obj`, `index`, and `value` are each evaluated once (left-to-right) on the
/// operand stack, then stashed into the DEDICATED HIGH scratch slots
/// `+14`/`+15`/`+16`, so no operand's sub-expression (a nested read or store)
/// can clobber an earlier one.
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
    // Evaluate the operand, the index jsval, and the assigned value on the
    // operand stack (so a nested member access in a later operand cannot clobber
    // an earlier one), then stash them into the dedicated high slots.
    emit_node(obj, function, scratch, strings, helpers);
    emit_node(index, function, scratch, strings, helpers);
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(sval));
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));
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
    // A primitive receiver (number/boolean, or a heap string or symbol) takes
    // no property: throw the strict-mode create-property error before any heap
    // access.
    if !never_primitive(obj) {
        emit_is_object(function, s0);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        emit_prop_throw_call(function, s0, s1, PROP_THROW_PRIMITIVE, helpers);
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }

    // A static named key (not an array index, not `length`) is an ordinary
    // property on every receiver, arrays included.
    if let IrNode::Str(name) = index
        && name != INDEX_LENGTH_KEY
        && !is_array_index(name)
    {
        emit_set_call(function, s0, index, s1, sval, strings, helpers);
        function.instruction(&Instruction::LocalGet(sval));
        return;
    }
    // `obj` is a heap pointer here (the guard above threw otherwise), so reading
    // its type byte is safe. Both arms can unwind, so the dispatch `if` is
    // tracked as a control frame.
    emit_load_type_byte(function, s0);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_index_set_array(function, s0, s1, sval, helpers);
    function.instruction(&Instruction::Else);
    emit_set_call(function, s0, index, s1, sval, strings, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // The assignment expression yields the stored value.
    function.instruction(&Instruction::LocalGet(sval));
}

/// Emit `__set(s0, ToPropertyKey(index), sval)` (the key interned) followed
/// by the pending-exception check.
fn emit_set_call(
    function: &mut Function,
    s0: u32,
    index: &IrNode,
    s1: u32,
    sval: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(s0));
    emit_prop_key(function, index, s1, true, strings, helpers);
    function.instruction(&Instruction::LocalGet(sval));
    function.instruction(&Instruction::Call(helpers.set));
    emit_propagate_if_pending(function, helpers);
}

/// Emit the array arm of [`emit_index_set_node`] for the array in `s0`: an
/// integer key `0 <= k < length` of a dense array whose slot is not a hole
/// stores the element at `elems + 8 + 8*k` inline (the compares run on `f64`,
/// so a non-number key fails them without reading memory); any other key (a
/// hole, or a `SPARSE` array) goes to `__array_set` (the prototype chain for
/// an element the array does not own, growth by index, sparse elements,
/// canonical index strings, `length`, named properties), followed by the
/// pending-exception check. The caller tracks the enclosing dispatch frame.
fn emit_index_set_array(
    function: &mut Function,
    s0: u32,
    s1: u32,
    sval: u32,
    helpers: Helpers<'_>,
) {
    // The address of element `i` (elems + 8*i; the slot MemArg adds the
    // data offset).
    let elem_addr = |function: &mut Function| {
        function.instruction(&Instruction::LocalGet(s0));
        function.instruction(&Instruction::I32WrapI64);
        emit_array_elems(function);
        function.instruction(&Instruction::LocalGet(s1));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::I32TruncSatF64U);
        function.instruction(&Instruction::I32Const(8));
        function.instruction(&Instruction::I32Mul);
        function.instruction(&Instruction::I32Add);
    };
    emit_dense_index_in_bounds(function, s0, s1);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    elem_addr(function);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    // An own element in bounds: store the element jsval at elems + 8 + 8*i.
    // Mirrors `emit_index_node`'s array arm.
    elem_addr(function);
    function.instruction(&Instruction::LocalGet(sval));
    function.instruction(&Instruction::I64Store(elem_slot(0)));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::LocalGet(sval));
    function.instruction(&Instruction::Call(helpers.array_set()));
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Call `__prop_throw(recv, key, mode)` on the receiver/key stashed in locals
/// `recv`/`key` and unwind like `throw`. The caller tracks every enclosing
/// control frame.
pub(super) fn emit_prop_throw_call(
    function: &mut Function,
    recv: u32,
    key: u32,
    mode: i32,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(recv));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32Const(mode));
    function.instruction(&Instruction::Call(helpers.prop_throw));
    emit_throw_return(function, helpers);
}

/// Emit a compound element/member assignment `obj[index] op= rhs`.
///
/// `obj` and `index` are evaluated exactly once into the stable temp locals
/// `obj_local`/`idx_local` (not scratch, which the read below clobbers), and an
/// object key is converted once ([`emit_key_to_primitive`]), so no
/// sub-expression side effect or user `toString` runs twice. The new value — current member read
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
    emit_key_to_primitive(function, idx_local, helpers);

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
            radix: None,
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

/// Whether `node` provably evaluates to an object/array/function heap value (a
/// fresh literal or instance), so a member store on it needs no
/// primitive-receiver guard.
fn never_primitive(node: &IrNode) -> bool {
    matches!(
        node,
        IrNode::Object { .. }
            | IrNode::Array(_)
            | IrNode::ArraySpread(_)
            | IrNode::Construct { .. }
            | IrNode::MapNew { .. }
            | IrNode::DateNew { .. }
            | IrNode::FuncValue { .. }
            | IrNode::DeclFunc(_)
    )
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
            | IrNode::Object { .. }
            | IrNode::Array(_)
            | IrNode::ArraySpread(_)
            | IrNode::Construct { .. }
            | IrNode::MapNew { .. }
            | IrNode::DateNew { .. }
            | IrNode::FuncValue { .. }
            | IrNode::DeclFunc(_)
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

/// `__nullish_throw(recv: i64, key: i64, set: i32)`: build the
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
pub(super) fn type_error_helper_locals() -> Vec<(u32, ValType)> {
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

/// Raise a `TypeError` whose message string jsval is in local `msg`:
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

    let error = IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Local(msg)),
        IrNode::Local(stack),
    );
    emit_node(&error, function, scratch, strings, helpers);
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
}

/// `__prop_throw(recv: i64, key: i64, mode: i32)`: build the
/// `TypeError` for a property store or delete the receiver cannot take, stash
/// it in `$exc_value`, set the pending flag and clear any routed completion.
/// [`PROP_THROW_PRIMITIVE`] is a store on
/// a primitive `recv` — `Cannot create property '<String(key)>' on
/// <number|string|boolean> '<String(recv)>'`; every other mode wraps
/// `String(key)` in its [`PROP_KEYED_MSGS`] fragments.
pub(super) fn emit_prop_throw(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let (recv, key, mode) = (0, 1, 2);
    let scratch = 3;
    let msg = scratch + 24;
    let mut function = Function::new(type_error_helper_locals());
    let at = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE);
    let concat = |function: &mut Function| {
        function.instruction(&Instruction::Call(helpers.concat));
    };
    let to_string = |function: &mut Function, local: u32| {
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
        function.instruction(&Instruction::Call(helpers.convert));
        function.instruction(&Instruction::I32WrapI64);
    };

    function.instruction(&Instruction::LocalGet(mode));
    function.instruction(&Instruction::I32Const(PROP_THROW_PRIMITIVE));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    // prefix + String(key) + "' on " + kind + " '" + String(recv) + "'", where
    // kind is number / symbol / string (the heap primitives) / boolean.
    function.instruction(&Instruction::I32Const(at(PRIM_CREATE_PREFIX)));
    to_string(&mut function, key);
    concat(&mut function);
    function.instruction(&Instruction::I32Const(at(PRIM_CREATE_MID)));
    concat(&mut function);
    function.instruction(&Instruction::I32Const(at("number")));
    function.instruction(&Instruction::I32Const(at("symbol")));
    function.instruction(&Instruction::I32Const(at("string")));
    emit_is_symbol(&mut function, recv);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::I32Const(at("boolean")));
    emit_is_pointer(&mut function, recv);
    function.instruction(&Instruction::Select);
    emit_is_number(&mut function, recv);
    function.instruction(&Instruction::Select);
    concat(&mut function);
    function.instruction(&Instruction::I32Const(at(PRIM_CREATE_QUOTE)));
    concat(&mut function);
    to_string(&mut function, recv);
    concat(&mut function);
    function.instruction(&Instruction::I32Const(at(PRIM_CREATE_SUFFIX)));
    concat(&mut function);
    function.instruction(&Instruction::Else);
    // prefix(mode) + String(key) + suffix(mode), each fragment picked from
    // PROP_KEYED_MSGS by a `select` chain on `mode`.
    let pick = |function: &mut Function,
                part: fn(&(i32, &'static str, &'static str)) -> &'static str| {
        let [first, rest @ ..] = &PROP_KEYED_MSGS;
        function.instruction(&Instruction::I32Const(at(part(first))));
        for entry in rest {
            function.instruction(&Instruction::I32Const(at(part(entry))));
            function.instruction(&Instruction::LocalGet(mode));
            function.instruction(&Instruction::I32Const(entry.0));
            function.instruction(&Instruction::I32Ne);
            function.instruction(&Instruction::Select);
        }
    };
    pick(&mut function, |entry| entry.1);
    to_string(&mut function, key);
    concat(&mut function);
    pick(&mut function, |entry| entry.2);
    concat(&mut function);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(msg));

    emit_raise_type_error(&mut function, msg, scratch, strings, helpers);
    function.instruction(&Instruction::End);
    function
}
