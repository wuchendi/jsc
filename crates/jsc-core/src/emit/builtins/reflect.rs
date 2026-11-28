//! The `Reflect` functions (DESIGN.md §6.11, ECMA-262 28.1). Each reads all
//! its arguments into locals at entry, before `ToPropertyKey` or any other
//! user code can run (a nested spread call would replace `$__spread_args`),
//! then throws for a non-object target and hands the work to the generic,
//! proxy-aware property helpers.

use super::object_static::{emit_element, own_key_list, require_proto};
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::ReflectFn;

/// The `TypeError` of `Reflect.apply` on a non-callable target.
const APPLY_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Reflect.apply target is not a function",
    "TypeError: Reflect.apply target is not a function",
];

/// The `TypeError` of `Reflect.construct` on a non-constructor target.
const CONSTRUCT_TARGET: CalleeErrorText<'static> = [
    "Reflect.construct target is not a constructor",
    "TypeError: Reflect.construct target is not a constructor",
];

/// The `TypeError` of `Reflect.construct` on a non-constructor `newTarget`.
const CONSTRUCT_NEW_TARGET: CalleeErrorText<'static> = [
    "Reflect.construct newTarget is not a constructor",
    "TypeError: Reflect.construct newTarget is not a constructor",
];

/// The `TypeError` of each function on a non-object target.
fn non_object(op: ReflectFn) -> CalleeErrorText<'static> {
    match op {
        ReflectFn::Apply | ReflectFn::Construct => [
            "Reflect called on non-object",
            "TypeError: Reflect called on non-object",
        ],
        ReflectFn::DefineProperty => [
            "Reflect.defineProperty called on non-object",
            "TypeError: Reflect.defineProperty called on non-object",
        ],
        ReflectFn::DeleteProperty => [
            "Reflect.deleteProperty called on non-object",
            "TypeError: Reflect.deleteProperty called on non-object",
        ],
        ReflectFn::Get => [
            "Reflect.get called on non-object",
            "TypeError: Reflect.get called on non-object",
        ],
        ReflectFn::GetOwnPropertyDescriptor => [
            "Reflect.getOwnPropertyDescriptor called on non-object",
            "TypeError: Reflect.getOwnPropertyDescriptor called on non-object",
        ],
        ReflectFn::GetPrototypeOf => [
            "Reflect.getPrototypeOf called on non-object",
            "TypeError: Reflect.getPrototypeOf called on non-object",
        ],
        ReflectFn::Has => [
            "Reflect.has called on non-object",
            "TypeError: Reflect.has called on non-object",
        ],
        ReflectFn::IsExtensible => [
            "Reflect.isExtensible called on non-object",
            "TypeError: Reflect.isExtensible called on non-object",
        ],
        ReflectFn::OwnKeys => [
            "Reflect.ownKeys called on non-object",
            "TypeError: Reflect.ownKeys called on non-object",
        ],
        ReflectFn::PreventExtensions => [
            "Reflect.preventExtensions called on non-object",
            "TypeError: Reflect.preventExtensions called on non-object",
        ],
        ReflectFn::Set => [
            "Reflect.set called on non-object",
            "TypeError: Reflect.set called on non-object",
        ],
        ReflectFn::SetPrototypeOf => [
            "Reflect.setPrototypeOf called on non-object",
            "TypeError: Reflect.setPrototypeOf called on non-object",
        ],
    }
}

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    APPLY_NOT_CALLABLE
        .into_iter()
        .chain(CONSTRUCT_TARGET)
        .chain(CONSTRUCT_NEW_TARGET)
        .chain(ReflectFn::ALL.into_iter().flat_map(non_object))
}

impl Helpers<'_> {
    /// Whether the program can hold `%Reflect%` (the §6.11 reachability
    /// gate).
    pub(in crate::emit) fn reflect(self) -> bool {
        self.reflect_gate == ReflectGate::Open
    }
}

/// Emit the body of the `Reflect` function `op`.
pub(super) fn emit(
    op: ReflectFn,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        ReflectFn::Apply => emit_apply(function, frame, strings, helpers),
        ReflectFn::Construct => emit_construct(function, frame, strings, helpers),
        ReflectFn::DefineProperty => emit_define_property(function, frame, strings, helpers),
        ReflectFn::OwnKeys => {
            target(function, frame, op, strings, helpers);
            own_key_list(function, frame, OWN_ALL_KEYS, strings, helpers);
        }
        ReflectFn::GetPrototypeOf => {
            let t = target(function, frame, op, strings, helpers);
            wasm!(function; LocalGet(t), Call(helpers.get_proto()));
        }
        ReflectFn::IsExtensible | ReflectFn::PreventExtensions => {
            let t = target(function, frame, op, strings, helpers);
            let r = frame.i32(0);
            let helper = if op == ReflectFn::IsExtensible {
                helpers.test_integrity()
            } else {
                helpers.set_integrity()
            };
            wasm!(function;
                LocalGet(t), I32Const(INTEGRITY_NON_EXTENSIBLE), Call(helper), LocalSet(r),
            );
            return_bool(function, r);
        }
        ReflectFn::SetPrototypeOf => emit_set_prototype_of(function, frame, strings, helpers),
        ReflectFn::DeleteProperty
        | ReflectFn::Get
        | ReflectFn::GetOwnPropertyDescriptor
        | ReflectFn::Has
        | ReflectFn::Set => emit_keyed(op, function, frame, strings, helpers),
    }
}

/// Read argument 0 into i64 `frame.i64(0)` and throw the non-object
/// `TypeError` of `op` unless it is an object; returns the local.
fn target(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    op: ReflectFn,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) -> u32 {
    let t = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(t));
    require_object(function, frame, t, non_object(op), strings, helpers);
    t
}

/// Throw `text` unless the jsval in `local` is an object.
fn require_object(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    local: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_object(function, local);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Return from the body when an exception is pending, else push the boolean
/// jsval of the i32 in `r` (nonzero is `true`).
fn return_bool(function: &mut FnBuilder<'_>, r: u32) {
    return_if_pending(function);
    wasm!(function; I64Const(TRUE_BITS), I64Const(FALSE_BITS), LocalGet(r), Select);
}

/// Push `1` when more than `n` arguments were passed.
fn passed_more_than(function: &mut FnBuilder<'_>, frame: Frame, n: u32) {
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(f64::from(n))), F64Gt,
    );
}

/// `Reflect.apply(target, thisArgument, argumentsList)`: `IsCallable(target)`,
/// then `CreateListFromArrayLike(argumentsList)` (`undefined` throws too),
/// then the call.
fn emit_apply(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (f, this, list, v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (n, src, dst, i) = (frame.i32(0), frame.base(1), frame.base(2), frame.i32(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(f));
    frame.arg(function, 1);
    wasm!(function; LocalSet(this));
    frame.arg(function, 2);
    wasm!(function; LocalSet(list));
    emit_is_function(function, f, helpers);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, APPLY_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    super::function::create_list(
        function,
        frame,
        [list, v],
        [src, dst, n, i],
        strings,
        helpers,
    );
    super::function::call_with_array(function, f, this, dst, n, helpers);
}

/// `Reflect.construct(target, argumentsList, newTarget = target)`:
/// `IsConstructor(target)`, then `IsConstructor(newTarget)` when one is
/// passed (`undefined` included), then `CreateListFromArrayLike`, then
/// `Construct(target, list, newTarget)` — the bound-function construct node,
/// so a non-`SELF_ALLOC` target allocates `this` from `Get(newTarget,
/// "prototype")` and a builtin constructor sees `newTarget` as `new.target`.
fn emit_construct(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    // `target`, the boxed list and `newTarget` sit where the bound construct
    // node reads them.
    let (f, args, nt) = (frame.i64(0), frame.i64(2), frame.i64(3));
    let (list, v) = (frame.y64(0), frame.y64(1));
    let (n, src, dst, i) = (frame.i32(0), frame.base(1), frame.base(2), frame.i32(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(f));
    frame.arg(function, 1);
    wasm!(function; LocalSet(list));
    passed_more_than(function, frame, 2);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    frame.arg(function, 2);
    wasm!(function; Else, LocalGet(f), End, LocalSet(nt));
    emit_is_constructor(function, f, helpers);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, CONSTRUCT_TARGET, strings, helpers);
    wasm!(function; End);
    emit_is_constructor(function, nt, helpers);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, CONSTRUCT_NEW_TARGET, strings, helpers);
    wasm!(function; End);
    super::function::create_list(
        function,
        frame,
        [list, v],
        [src, dst, n, i],
        strings,
        helpers,
    );
    wasm!(function; LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(args));
    emit_node(
        &super::function::bound_construct(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}

/// `Reflect.defineProperty(target, propertyKey, attributes)`:
/// `ToPropertyKey`, `ToPropertyDescriptor`, then `[[DefineOwnProperty]]`'s
/// boolean (a rejection returns `false`; an array `length` `RangeError` or a
/// trap's throw propagates).
fn emit_define_property(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (kv, attrs) = (frame.i64(1), frame.i64(2));
    let (key, list, j, r) = (frame.base(0), frame.base(1), frame.i32(2), frame.i32(3));
    frame.arg(function, 1);
    wasm!(function; LocalSet(kv));
    frame.arg(function, 2);
    wasm!(function; LocalSet(attrs));
    let t = target(function, frame, ReflectFn::DefineProperty, strings, helpers);
    wasm!(function; LocalGet(kv), I32Const(1), Call(helpers.to_key), LocalSet(key));
    shake::note_runtime_key();
    return_if_pending(function);
    wasm!(function; I32Const(0), Call(helpers.new_array()), LocalSet(list));
    return_if_pending(function);
    wasm!(function; LocalGet(attrs), LocalGet(list), Call(helpers.to_desc()));
    return_if_pending(function);
    wasm!(function; I32Const(0), LocalSet(j), LocalGet(t), I32WrapI64, LocalGet(key));
    emit_element(function, list, j, 0);
    wasm!(function; F64ReinterpretI64, I32TruncSatF64U);
    for slot in 1..4 {
        emit_element(function, list, j, slot);
    }
    wasm!(function;
        Call(helpers.define_own()), I32Const(DEFINE_OK), I32Eq, LocalSet(r),
    );
    return_bool(function, r);
}

/// `Reflect.setPrototypeOf(target, proto)`: `proto` must be an object or
/// `null`; then `[[SetPrototypeOf]]`'s boolean (any rejection is `false`).
fn emit_set_prototype_of(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (proto, r) = (frame.i64(1), frame.i32(0));
    frame.arg(function, 1);
    wasm!(function; LocalSet(proto));
    let t = target(function, frame, ReflectFn::SetPrototypeOf, strings, helpers);
    require_proto(function, proto, helpers);
    wasm!(function;
        LocalGet(t), I32WrapI64, LocalGet(proto), Call(helpers.set_proto()), I32Eqz, LocalSet(r),
    );
    return_bool(function, r);
}

/// The functions over one property key: `deleteProperty(t, k)`, `get(t, k,
/// receiver = t)`, `getOwnPropertyDescriptor(t, k)`, `has(t, k)` and `set(t,
/// k, v, receiver = t)`. The target check comes first, then
/// `ToPropertyKey` (a `set` interns the key, the others only look it up).
fn emit_keyed(
    op: ReflectFn,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (kv, value, recv) = (frame.i64(1), frame.i64(2), frame.i64(3));
    let (key, r) = (frame.base(0), frame.i32(0));
    // The receiver is argument 2 of `get`, 3 of `set`.
    let recv_at = if op == ReflectFn::Set { 3 } else { 2 };
    frame.arg(function, 1);
    wasm!(function; LocalSet(kv));
    if op == ReflectFn::Set {
        frame.arg(function, 2);
        wasm!(function; LocalSet(value));
    }
    if matches!(op, ReflectFn::Get | ReflectFn::Set) {
        passed_more_than(function, frame, recv_at);
        wasm!(function; If(BlockType::Result(ValType::I64)));
        frame.arg(function, recv_at);
        wasm!(function; Else);
        frame.arg(function, 0);
        wasm!(function; End, LocalSet(recv));
    }
    let t = target(function, frame, op, strings, helpers);
    let insert = i32::from(op == ReflectFn::Set);
    wasm!(function; LocalGet(kv), I32Const(insert), Call(helpers.to_key), LocalSet(key));
    // `get` and `getOwnPropertyDescriptor` hand out the value under a
    // run-time key (a builtin no body names); `has`, `deleteProperty` and
    // `set` only answer a boolean or store the program's own value.
    if matches!(op, ReflectFn::Get | ReflectFn::GetOwnPropertyDescriptor) {
        shake::note_runtime_key();
    }
    return_if_pending(function);
    match op {
        ReflectFn::DeleteProperty => {
            wasm!(function;
                LocalGet(t), LocalGet(key), I32Const(0), Call(helpers.delete), LocalSet(r),
            );
            return_bool(function, r);
        }
        ReflectFn::Get => {
            wasm!(function; LocalGet(t), LocalGet(key), LocalGet(recv), Call(helpers.super_get()));
        }
        ReflectFn::GetOwnPropertyDescriptor => {
            wasm!(function; LocalGet(t), LocalGet(key), Call(helpers.own_desc()));
        }
        ReflectFn::Has => {
            wasm!(function; LocalGet(key), LocalGet(t), Call(helpers.has), LocalSet(r));
            return_bool(function, r);
        }
        _ => {
            wasm!(function;
                LocalGet(t), LocalGet(key), LocalGet(value), LocalGet(recv),
                Call(helpers.set_bool()), LocalSet(r),
            );
            return_bool(function, r);
        }
    }
}
