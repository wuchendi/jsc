//! `%Proxy%`, `Proxy.revocable` and the revoke function (DESIGN.md §6.11).
//! A proxy is a [`abi::TYPE_PROXY`] block; its internal methods live in
//! `emit/props/proxy.rs`.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::ProxyOp;

/// The `TypeError` of `Proxy(..)` called without `new`.
const NOT_NEW: CalleeErrorText<'static> = [
    "Constructor Proxy requires 'new'",
    "TypeError: Constructor Proxy requires 'new'",
];

/// The `TypeError` of a non-object target or handler.
const NON_OBJECT: CalleeErrorText<'static> = [
    "Cannot create proxy with a non-object as target or handler",
    "TypeError: Cannot create proxy with a non-object as target or handler",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    NOT_NEW
        .into_iter()
        .chain(NON_OBJECT)
        .chain(["proxy", "revoke"])
        .chain(proxy_strings())
}

fn m8(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

fn m32(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    }
}

fn m64(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 3,
        memory_index: 0,
    }
}

/// Push nonzero when the jsval in `local` is a proxy whose header byte
/// `offset` has a bit of `mask`.
fn proxy_bits(function: &mut FnBuilder<'_>, local: u32, offset: u32, mask: u8) {
    emit_is_pointer(function, local);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64, I32Load8U(m8(0)), I32Const(TYPE_PROXY), I32Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64, I32Load8U(m8(offset)), I32Const(i32::from(mask)), I32And,
        Else, I32Const(0), End,
        Else, I32Const(0), End,
    );
}

/// `ProxyCreate(target, handler)` (ECMA-262 10.5.14) into i64 `out`: a
/// non-object target or handler throws; else a fresh proxy block whose
/// constructor flags and callable class byte follow the target's.
fn create(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    [target, handler, out]: [u32; 3],
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let b = frame.base(0);
    let ctor = i32::from(abi::FUNC_FLAG_CONSTRUCTOR | abi::FUNC_FLAG_SELF_ALLOC) << 16;
    let callable = i32::from(abi::PROXY_CLASS_CALLABLE) << 24;
    emit_is_object(function, target);
    emit_is_object(function, handler);
    wasm!(function; I32And, I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NON_OBJECT, strings, helpers);
    wasm!(function; End);
    emit_alloc_const(function, helpers, abi::PROXY_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalSet(b),
        LocalGet(b),
        I32Const(TYPE_PROXY | ctor), I32Const(TYPE_PROXY),
    );
    emit_is_constructor(function, target);
    proxy_bits(
        function,
        target,
        abi::HEADER_FLAGS_OFFSET,
        abi::FUNC_FLAG_CONSTRUCTOR,
    );
    wasm!(function; I32Or, Select, I32Const(callable), I32Const(0));
    emit_is_function(function, target);
    proxy_bits(
        function,
        target,
        abi::HEADER_CLASS_OFFSET,
        abi::PROXY_CLASS_CALLABLE,
    );
    wasm!(function;
        I32Or, Select, I32Or, I32Store(m32(0)),
        LocalGet(b), I64Const(NULL_BITS), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(b), I64Const(UNDEFINED_BITS), I64Store(m64(abi::FUNC_ENV_OFFSET)),
        LocalGet(b), LocalGet(target), I64Store(m64(abi::PROXY_TARGET_OFFSET)),
        LocalGet(b), LocalGet(handler), I64Store(m64(abi::PROXY_HANDLER_OFFSET)),
        LocalGet(b), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
    );
}

/// Emit the body of the `Proxy` builtin `op`.
pub(super) fn emit(
    op: ProxyOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        ProxyOp::Ctor => emit_ctor(function, frame, strings, helpers),
        ProxyOp::Revocable => emit_revocable(function, frame, strings, helpers),
        ProxyOp::Revoke => emit_revoke(function, frame),
    }
}

/// `%Proxy%` (`new Proxy(target, handler)`): `ProxyCreate`; a call without
/// `new` throws.
fn emit_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (target, handler, out) = (frame.i64(0), frame.i64(1), frame.i64(2));
    emit_is_construct_call(function, frame.argc());
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_NEW, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 0);
    wasm!(function; LocalSet(target));
    frame.arg(function, 1);
    wasm!(function; LocalSet(handler));
    create(function, frame, [target, handler, out], strings, helpers);
    wasm!(function; LocalGet(out));
}

/// `Proxy.revocable(target, handler)`: `ProxyCreate`, then a fresh ordinary
/// object with own `proxy` and `revoke` (a fresh revoke function whose `env`
/// is the proxy).
fn emit_revocable(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (target, handler, proxy, revoke) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let o = frame.base(1);
    frame.arg(function, 0);
    wasm!(function; LocalSet(target));
    frame.arg(function, 1);
    wasm!(function; LocalSet(handler));
    create(function, frame, [target, handler, proxy], strings, helpers);
    let slot = helpers.builtin_slot(Builtin::Proxy(ProxyOp::Revoke));
    emit_settler_value(slot, proxy, function, frame.scratch(), helpers);
    wasm!(function; LocalSet(revoke));
    emit_alloc_const(function, helpers, abi::OBJ_PREFIX_SIZE.cast_signed());
    wasm!(function; LocalSet(o));
    emit_object_prefix(
        function,
        o,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    for (key, value) in [("proxy", proxy), ("revoke", revoke)] {
        wasm!(function;
            LocalGet(o), I32Const(static_key(strings, key)), LocalGet(value),
            I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
        );
    }
    wasm!(function; LocalGet(o), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// The revoke function: clear its proxy's target and handler (idempotent),
/// return `undefined`.
fn emit_revoke(function: &mut FnBuilder<'_>, frame: Frame) {
    let env = frame.k;
    emit_is_pointer(function, env);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(env), I32WrapI64, I32Load8U(m8(0)), I32Const(TYPE_PROXY), I32Eq,
        If(BlockType::Empty),
        LocalGet(env), I32WrapI64, I64Const(NULL_BITS), I64Store(m64(abi::PROXY_TARGET_OFFSET)),
        LocalGet(env), I32WrapI64, I64Const(NULL_BITS), I64Store(m64(abi::PROXY_HANDLER_OFFSET)),
        End,
        End,
        I64Const(UNDEFINED_BITS),
    );
}
