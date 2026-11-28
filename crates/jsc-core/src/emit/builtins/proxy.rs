//! `%Proxy%`, `Proxy.revocable` and the revoke function (DESIGN.md §6.11).
//! A proxy is a [`abi::TYPE_PROXY`] block; its internal methods live in
//! `emit/props/proxy.rs`.

use super::array::push_call;
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

/// The `TypeError` of a `construct` trap returning a non-object.
const CONSTRUCT_NON_OBJECT: CalleeErrorText<'static> = [
    "proxy [[Construct]] must return an object",
    "TypeError: proxy [[Construct]] must return an object",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    NOT_NEW
        .into_iter()
        .chain(NON_OBJECT)
        .chain(CONSTRUCT_NON_OBJECT)
        .chain(["proxy", "revoke", "apply", "construct"])
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
    let (b, c) = (frame.base(0), frame.i32(0));
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
    emit_is_constructor(function, target, helpers);
    wasm!(function; Select, I32Const(callable), I32Const(0));
    emit_is_function(function, target, helpers);
    wasm!(function;
        LocalTee(c), Select, I32Or, I32Store(m32(0)),
        LocalGet(b), I64Const(NULL_BITS), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(b), LocalGet(target), I64Store(m64(abi::PROXY_TARGET_OFFSET)),
        LocalGet(b), LocalGet(handler), I64Store(m64(abi::PROXY_HANDLER_OFFSET)),
        LocalGet(b), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
        // A callable proxy calls through the trampoline with itself as `env`.
        LocalGet(b),
        LocalGet(out), I64Const(UNDEFINED_BITS), LocalGet(c), Select,
        I64Store(m64(abi::FUNC_ENV_OFFSET)),
        LocalGet(b),
        I32Const(helpers.builtin_slot(Builtin::Proxy(ProxyOp::Call)).cast_signed()),
        I32Const(0), LocalGet(c), Select,
        I32Store(m32(abi::FUNC_INDEX_OFFSET)),
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
        ProxyOp::Call => emit_call(function, frame, strings, helpers),
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

/// The trampoline of every callable proxy (`env` = the proxy; ECMA-262
/// 10.5.12-13). Before any user code runs it snapshots `this` and
/// `new.target` and copies the arguments (the `K` slots, then the published
/// spread array past them) into a fresh array. It reads the target and the
/// handler, then `GetMethod(handler, "apply" | "construct")` (a revoked
/// proxy throws). A call without a trap calls the target with the array; a
/// construct without one constructs the target with the incoming
/// `new.target`. A trap is called with `this = handler` and `« target,
/// thisArgument, argsArray »` / `« target, argsArray, newTarget »`; a
/// `construct` trap's result must be an object.
fn emit_call(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    // `target`, `args` and `new.target` sit where the bound construct node
    // reads them.
    let (target, this, args, nt) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (handler, trap) = (frame.y64(0), frame.y64(1));
    let (n, dst, i) = (frame.i32(0), frame.base(1), frame.i32(2));
    let (shift, c, tmp) = (frame.x32(0), frame.f64(0), frame.y64(2));
    let env = frame.k;
    wasm!(function;
        GlobalGet(THIS_GLOBAL), LocalSet(this),
        GlobalGet(NEW_TARGET_GLOBAL), LocalSet(nt),
        // n = argc, decoding the construct marker -(n + 1).
        LocalGet(frame.argc()), F64ReinterpretI64, LocalSet(c),
        F64Const(Ieee64::from(-1.0)), LocalGet(c), F64Sub,
        LocalGet(c), LocalGet(c), F64Const(Ieee64::from(0.0)), F64Lt, Select,
        I32TruncSatF64U, LocalTee(n), Call(helpers.new_array()), LocalSet(dst),
    );
    return_if_pending(function);
    wasm!(function; I32Const(0), LocalSet(shift));
    super::function::store_args(function, frame, dst, shift, 0, n, i, tmp);
    wasm!(function;
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(args),
        LocalGet(env), I32WrapI64, I64Load(m64(abi::PROXY_TARGET_OFFSET)), LocalSet(target),
        LocalGet(env), I32WrapI64, I64Load(m64(abi::PROXY_HANDLER_OFFSET)), LocalSet(handler),
        LocalGet(c), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty),
        LocalGet(env), I32WrapI64, I32Const(static_key(strings, "construct")),
        Call(helpers.proxy_trap()), LocalSet(trap),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(trap), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
    );
    emit_node(
        &super::function::bound_construct(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; Return, End);
    push_call(
        function,
        frame,
        trap,
        handler,
        &[CallArg::Val(target), CallArg::Val(args), CallArg::Val(nt)],
        helpers,
    );
    wasm!(function; LocalSet(trap));
    return_if_pending(function);
    emit_is_object(function, trap);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, CONSTRUCT_NON_OBJECT, strings, helpers);
    wasm!(function;
        End,
        LocalGet(trap), Return,
        End,
        LocalGet(env), I32WrapI64, I32Const(static_key(strings, "apply")),
        Call(helpers.proxy_trap()), LocalSet(trap),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(trap), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I64)),
    );
    super::function::call_with_array(function, target, this, dst, n, helpers);
    wasm!(function; Else);
    push_call(
        function,
        frame,
        trap,
        handler,
        &[CallArg::Val(target), CallArg::Val(this), CallArg::Val(args)],
        helpers,
    );
    wasm!(function; End);
}
