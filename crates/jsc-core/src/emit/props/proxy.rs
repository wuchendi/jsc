//! Proxy exotic objects (DESIGN.md §6.11): the trap protocol behind every
//! property helper's proxy path.
//!
//! - `__proxy_trap(p, name)` is `ValidateNonRevokedProxy` + `GetMethod(handler,
//!   name)`;
//! - one `__proxy_<m>` helper per object internal method (ECMA-262 §10.5.1 ..
//!   §10.5.11): it reads the target first, looks the trap up, forwards to the
//!   target's (proxy-aware) generic helper when there is none, else calls the
//!   trap with `this = handler` and checks every invariant (a `TypeError`
//!   each);
//! - `__get_proto(v)` is `[[GetPrototypeOf]]` of any object.
//!
//! A helper returning `i32` returns `0` and one returning `i64` `undefined`
//! when it leaves an exception pending; callers test `$exc_pending`.

#[allow(clippy::wildcard_imports)]
use super::*;
use builtins::{CallArg, call_spilling, emit_with_array_length};

/// Kind byte of a proxy (see [`abi::TYPE_PROXY`]).
pub(in crate::emit) const TYPE_PROXY: i32 = abi::TYPE_PROXY as i32;
/// The `[[ProxyTarget]]` slot.
const TARGET: u64 = abi::PROXY_TARGET_OFFSET as u64;
/// The `[[ProxyHandler]]` slot.
const HANDLER: u64 = abi::PROXY_HANDLER_OFFSET as u64;
/// A descriptor record's size in bytes: four jsvals.
const RECORD: i32 = 32;

/// The trap names, also the `key` of the non-keyed `TypeError`s.
const TRAPS: [&str; 11] = [
    "getPrototypeOf",
    "setPrototypeOf",
    "isExtensible",
    "preventExtensions",
    "getOwnPropertyDescriptor",
    "defineProperty",
    "has",
    "get",
    "set",
    "deleteProperty",
    "ownKeys",
];

/// The operation name of `IsArray`'s revoked-proxy `TypeError`.
const IS_ARRAY: &str = "IsArray";

/// The static strings the proxy helpers reference.
pub(in crate::emit) fn proxy_strings() -> impl Iterator<Item = &'static str> {
    TRAPS.into_iter().chain([
        IS_ARRAY,
        INDEX_LENGTH_KEY,
        "value",
        "writable",
        "get",
        "set",
        "enumerable",
        "configurable",
    ])
}

/// An emitter pushing one value.
type Push<'a> = &'a dyn Fn(&mut FnBuilder<'_>);

/// What a helper returns when it leaves an exception pending.
#[derive(Clone, Copy)]
enum Ret {
    /// `0`.
    I32,
    /// `undefined`.
    I64,
}

impl Ret {
    fn fail(self) -> Instruction<'static> {
        match self {
            Self::I32 => Instruction::I32Const(0),
            Self::I64 => Instruction::I64Const(UNDEFINED_BITS),
        }
    }
}

/// The locals every trap helper declares after its parameters.
#[derive(Clone, Copy)]
struct L {
    /// The target (jsval).
    t: u32,
    /// The handler (jsval).
    h: u32,
    /// The trap (jsval).
    tr: u32,
    /// The trap result (jsval).
    r: u32,
    /// The spread argument array of a trap call past `K` (jsval).
    spill: u32,
    /// A record list (rooted base).
    list: u32,
    /// Raw i32 scratch.
    a: u32,
    b: u32,
    c: u32,
    e: u32,
    ext: u32,
    /// The boxed key (jsval).
    kj: u32,
    /// Jsval scratch.
    x: u32,
}

/// The classes of [`L`], in order.
const COMMON: [LocalClass; 13] = [
    ROOT_JS, ROOT_JS, ROOT_JS, ROOT_JS, ROOT_JS, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32, RAW_I32,
    RAW_I32, ROOT_JS, ROOT_JS,
];

/// A helper builder over `params` followed by the [`COMMON`] locals and
/// `extra`.
fn builder<'a>(
    leaves: &'a LeafSet,
    params: &[LocalClass],
    extra: &[LocalClass],
) -> (FnBuilder<'a>, L, u32) {
    let mut locals = COMMON.to_vec();
    locals.extend_from_slice(extra);
    let f = helper_builder(leaves, params, &locals);
    let n = u32::try_from(params.len()).unwrap_or(0);
    let l = L {
        t: n,
        h: n + 1,
        tr: n + 2,
        r: n + 3,
        spill: n + 4,
        list: n + 5,
        a: n + 6,
        b: n + 7,
        c: n + 8,
        e: n + 9,
        ext: n + 10,
        kj: n + 11,
        x: n + 12,
    };
    (f, l, n + 13)
}

/// Return the failure value when an exception is pending.
fn check(f: &mut FnBuilder<'_>, ret: Ret) {
    wasm!(f; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    f.instruction(&ret.fail());
    wasm!(f; Return, End);
}

/// Read the target and handler of the proxy in i32 `p`, then look up the
/// trap `name` (a revoked proxy or a non-callable trap throws).
fn prologue(
    f: &mut FnBuilder<'_>,
    l: L,
    p: u32,
    name: &str,
    ret: Ret,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(f;
        LocalGet(p), I64Load(m64(TARGET)), LocalSet(l.t),
        LocalGet(p), I64Load(m64(HANDLER)), LocalSet(l.h),
        LocalGet(p), I32Const(static_key(strings, name)), Call(helpers.proxy_trap()), LocalSet(l.tr),
    );
    check(f, ret);
}

/// Emit `forward` and return its result when the trap is absent.
fn if_no_trap(f: &mut FnBuilder<'_>, l: L, forward: impl FnOnce(&mut FnBuilder<'_>)) {
    wasm!(f; LocalGet(l.tr), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
    forward(f);
    wasm!(f; Return, End);
}

/// Call the trap with `this = handler` over `args` (every one passed, the
/// surplus past `K` in a spread argument array) into `l.r`.
fn call_trap(f: &mut FnBuilder<'_>, l: L, args: &[CallArg], ret: Ret, helpers: Helpers<'_>) {
    call_spilling(f, helpers.max_args, l.spill, l.tr, Some(l.h), args, helpers);
    wasm!(f; LocalSet(l.r));
    check(f, ret);
}

/// Push `ToBoolean` of the jsval in `local` (`0` / `1`).
fn to_boolean(f: &mut FnBuilder<'_>, local: u32, strings: &StrOffsets, helpers: Helpers<'_>) {
    emit_cond(&IrNode::Local(local), f, 0, strings, helpers);
}

/// Raise the `__prop_throw` `TypeError` of `mode` keyed by the interned key
/// in i32 `key`, then return the failure value.
fn throw_key(f: &mut FnBuilder<'_>, key: u32, mode: i32, ret: Ret, helpers: Helpers<'_>) {
    wasm!(f; I64Const(UNDEFINED_BITS), LocalGet(key));
    emit_box_heap(f);
    wasm!(f; I32Const(mode), Call(helpers.prop_throw));
    f.instruction(&ret.fail());
    wasm!(f; Return);
}

/// Raise the `__prop_throw` `TypeError` of `mode` keyed by the trap `name`,
/// then return the failure value.
fn throw_name(
    f: &mut FnBuilder<'_>,
    name: &str,
    mode: i32,
    ret: Ret,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(f;
        I64Const(UNDEFINED_BITS), I64Const(HEAP_BITS | i64::from(static_key(strings, name))),
        I32Const(mode), Call(helpers.prop_throw),
    );
    f.instruction(&ret.fail());
    wasm!(f; Return);
}

/// `IsExtensible(target)` into `l.ext`.
fn target_extensible(f: &mut FnBuilder<'_>, l: L, ret: Ret, helpers: Helpers<'_>) {
    wasm!(f; LocalGet(l.t), I32Const(INTEGRITY_NON_EXTENSIBLE), Call(helpers.test_integrity()), LocalSet(l.ext));
    check(f, ret);
}

/// `target.[[GetOwnProperty]](key)` into a fresh `l.list`: `l.a` = present,
/// `l.b` = the list's element store (the record at `+0`), `l.e` = its
/// `desc` (when present).
fn target_desc(f: &mut FnBuilder<'_>, l: L, key: u32, ret: Ret, helpers: Helpers<'_>) {
    wasm!(f;
        I32Const(0), Call(helpers.new_array()), LocalSet(l.list),
        LocalGet(l.t), LocalGet(key), LocalGet(l.list), Call(helpers.own_prop()), LocalSet(l.a),
    );
    check(f, ret);
    wasm!(f; LocalGet(l.list), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalSet(l.b));
    wasm!(f; LocalGet(l.a), If(BlockType::Empty));
    record_desc(f, l.b, 0);
    wasm!(f; LocalSet(l.e), End);
}

/// Push the `desc` of record `index` of the element store in i32 `store`.
fn record_desc(f: &mut FnBuilder<'_>, store: u32, index: u64) {
    wasm!(f;
        LocalGet(store), I64Load(m64(ELEMS_DATA_OFFSET + 32 * index)),
        F64ReinterpretI64, I32TruncSatF64U,
    );
}

/// Push field `field` (`1` value, `2` get, `3` set) of record `index` of the
/// element store in i32 `store`.
fn record_field(f: &mut FnBuilder<'_>, store: u32, index: u64, field: u64) {
    wasm!(f; LocalGet(store), I64Load(m64(ELEMS_DATA_OFFSET + 32 * index + 8 * field)));
}

/// Append the number `desc` pushed by `push` to the array in i32 `list`.
fn push_desc(
    f: &mut FnBuilder<'_>,
    list: u32,
    push: impl FnOnce(&mut FnBuilder<'_>),
    helpers: Helpers<'_>,
) {
    wasm!(f; LocalGet(list));
    push(f);
    wasm!(f; F64ConvertI32U, I64ReinterpretF64, Call(helpers.array_push), Drop);
}

/// `__proxy_trap(p: i32, name: i32) -> i64`: `ValidateNonRevokedProxy` and
/// `GetMethod(handler, name)` for the proxy `p` and the interned trap name:
/// a revoked proxy throws `PROXY_REVOKED`; the trap is read with `__get`
/// (the handler may itself be a proxy); `undefined` / `null` read as
/// `undefined`; any other non-callable value throws `PROXY_TRAP`.
pub(in crate::emit) fn emit_proxy_trap(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32], &[ROOT_JS, ROOT_JS]);
    let (p, name, h, v) = (0, 1, 2, 3);
    let throw = |f: &mut FnBuilder<'_>, mode: i32| {
        wasm!(f; I64Const(UNDEFINED_BITS), LocalGet(name));
        emit_box_heap(f);
        wasm!(f; I32Const(mode), Call(helpers.prop_throw), I64Const(UNDEFINED_BITS), Return);
    };
    wasm!(f;
        LocalGet(p), I64Load(m64(HANDLER)), LocalTee(h), I64Const(NULL_BITS), I64Eq,
        If(BlockType::Empty),
    );
    throw(&mut f, PROP_THROW_PROXY_REVOKED);
    wasm!(f;
        End,
        LocalGet(h), LocalGet(name), Call(helpers.get), LocalSet(v),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(v), I64Const(UNDEFINED_BITS), I64Eq, LocalGet(v), I64Const(NULL_BITS), I64Eq, I32Or,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
    emit_is_function(&mut f, v, helpers);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    throw(&mut f, PROP_THROW_PROXY_TRAP);
    wasm!(f; End, LocalGet(v), End);
    finish_builder(f)
}

/// `__get_proto(v: i64) -> i64`: `v.[[GetPrototypeOf]]()` of the object `v`:
/// its `proto`, or a proxy's `getPrototypeOf` trap.
pub(in crate::emit) fn emit_get_proto(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS], &[RAW_I32]);
    let (v, o) = (0, 1);
    wasm!(f; LocalGet(v), I32WrapI64, LocalSet(o));
    emit_if_proxy(&mut f, o, helpers, |f| {
        wasm!(f; LocalGet(o), Call(helpers.proxy_get_proto()), Return);
    });
    wasm!(f; LocalGet(o), I64Load(m64(OBJ_PROTO_OFFSET)), End);
    finish_builder(f)
}

/// `__is_array(v: i64) -> i32`: `IsArray(v)` (ECMA-262 7.2.2): `1` for a
/// `TYPE_ARRAY` block; a proxy answers for its target (repeatedly, for proxy
/// chains), a revoked one throws `PROXY_REVOKED` (key `"IsArray"`) and
/// returns `0`; anything else is `0`.
pub(in crate::emit) fn emit_is_array(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS], &[RAW_I32]);
    let (v, o) = (0, 1);
    wasm!(f; Loop(BlockType::Empty));
    emit_is_pointer(&mut f, v);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(v), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty), I32Const(1), Return, End,
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_PROXY), I32Eq,
        If(BlockType::Empty),
        LocalGet(o), I64Load(m64(HANDLER)), I64Const(NULL_BITS), I64Eq,
        If(BlockType::Empty),
    );
    throw_name(
        &mut f,
        IS_ARRAY,
        PROP_THROW_PROXY_REVOKED,
        Ret::I32,
        strings,
        helpers,
    );
    wasm!(f;
        End,
        LocalGet(o), I64Load(m64(TARGET)), LocalSet(v), Br(1),
        End,
        End,
        I32Const(0), End,
    );
    finish_builder(f)
}

/// `__proxy_get_proto(p: i32) -> i64` (§10.5.1): the trap result must be an
/// object or `null`, and equal the target's prototype when the target is not
/// extensible.
pub(in crate::emit) fn emit_proxy_get_proto(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let name = "getPrototypeOf";
    let ret = Ret::I64;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE], &[]);
    let p = 0;
    prologue(&mut f, l, p, name, ret, strings, helpers);
    if_no_trap(
        &mut f,
        l,
        |f| wasm!(f; LocalGet(l.t), Call(helpers.get_proto())),
    );
    call_trap(&mut f, l, &[CallArg::Val(l.t)], ret, helpers);
    emit_is_object(&mut f, l.r);
    wasm!(f; LocalGet(l.r), I64Const(NULL_BITS), I64Eq, I32Or, I32Eqz, If(BlockType::Empty));
    throw_name(
        &mut f,
        name,
        PROP_THROW_PROXY_INVARIANT,
        ret,
        strings,
        helpers,
    );
    wasm!(f; End);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f;
        LocalGet(l.ext), If(BlockType::Empty), LocalGet(l.r), Return, End,
        LocalGet(l.t), Call(helpers.get_proto()), LocalSet(l.x),
    );
    check(&mut f, ret);
    wasm!(f; LocalGet(l.r), LocalGet(l.x), I64Ne, If(BlockType::Empty));
    throw_name(
        &mut f,
        name,
        PROP_THROW_PROXY_INVARIANT,
        ret,
        strings,
        helpers,
    );
    wasm!(f; End, LocalGet(l.r), End);
    finish_builder(f)
}

/// `__proxy_set_proto(p: i32, v: i64) -> i32` (§10.5.2): `ToBoolean` of the
/// trap result; a true one over a non-extensible target needs `v` to be the
/// target's prototype.
pub(in crate::emit) fn emit_proxy_set_proto(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let name = "setPrototypeOf";
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE, ROOT_JS], &[]);
    let (p, v) = (0, 1);
    prologue(&mut f, l, p, name, ret, strings, helpers);
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), I32WrapI64, LocalGet(v), Call(helpers.set_proto()), I32Eqz);
    });
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(v)],
        ret,
        helpers,
    );
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; I32Eqz, If(BlockType::Empty), I32Const(0), Return, End);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f;
        LocalGet(l.ext), If(BlockType::Empty), I32Const(1), Return, End,
        LocalGet(l.t), Call(helpers.get_proto()), LocalSet(l.x),
    );
    check(&mut f, ret);
    wasm!(f; LocalGet(v), LocalGet(l.x), I64Ne, If(BlockType::Empty));
    throw_name(
        &mut f,
        name,
        PROP_THROW_PROXY_INVARIANT,
        ret,
        strings,
        helpers,
    );
    wasm!(f; End, I32Const(1), End);
    finish_builder(f)
}

/// `__proxy_is_extensible(p: i32) -> i32` (§10.5.3): `ToBoolean` of the trap
/// result, which must equal `IsExtensible(target)`.
pub(in crate::emit) fn emit_proxy_is_extensible(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let name = "isExtensible";
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE], &[]);
    let p = 0;
    prologue(&mut f, l, p, name, ret, strings, helpers);
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), I32Const(INTEGRITY_NON_EXTENSIBLE), Call(helpers.test_integrity()));
    });
    call_trap(&mut f, l, &[CallArg::Val(l.t)], ret, helpers);
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; LocalSet(l.a));
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f; LocalGet(l.a), LocalGet(l.ext), I32Ne, If(BlockType::Empty));
    throw_name(
        &mut f,
        name,
        PROP_THROW_PROXY_INVARIANT,
        ret,
        strings,
        helpers,
    );
    wasm!(f; End, LocalGet(l.a), End);
    finish_builder(f)
}

/// `__proxy_prevent_ext(p: i32) -> i32` (§10.5.4): `ToBoolean` of the trap
/// result; a true one needs a non-extensible target.
pub(in crate::emit) fn emit_proxy_prevent_ext(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let name = "preventExtensions";
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE], &[]);
    let p = 0;
    prologue(&mut f, l, p, name, ret, strings, helpers);
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), I32Const(INTEGRITY_NON_EXTENSIBLE), Call(helpers.set_integrity()));
    });
    call_trap(&mut f, l, &[CallArg::Val(l.t)], ret, helpers);
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; LocalTee(l.a), If(BlockType::Empty));
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f; LocalGet(l.ext), If(BlockType::Empty));
    throw_name(
        &mut f,
        name,
        PROP_THROW_PROXY_INVARIANT,
        ret,
        strings,
        helpers,
    );
    wasm!(f; End, End, LocalGet(l.a), End);
    finish_builder(f)
}

/// `__proxy_own_prop(p: i32, key: i32, out: i32) -> i32` (§10.5.5): the trap
/// result must be an object or `undefined`; `undefined` hides only a
/// configurable property of an extensible target; an object is
/// `ToPropertyDescriptor`ed, completed, checked with `__desc_compatible`
/// against the target's property, and a non-configurable result needs a
/// non-configurable target property (non-writable too when the result is).
/// A present result's complete record is appended to `out`.
pub(in crate::emit) fn emit_proxy_own_prop(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I32;
    let (mut f, l, d) = builder(leaves, &[ROOT_BASE, ROOT_BASE, ROOT_BASE], &[RAW_I32]);
    let (p, key, out) = (0, 1, 2);
    let invariant =
        |f: &mut FnBuilder<'_>| throw_key(f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    prologue(
        &mut f,
        l,
        p,
        "getOwnPropertyDescriptor",
        ret,
        strings,
        helpers,
    );
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), LocalGet(key), LocalGet(out), Call(helpers.own_prop()));
    });
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(l.kj)],
        ret,
        helpers,
    );
    emit_is_object(&mut f, l.r);
    wasm!(f;
        I32Eqz, LocalGet(l.r), I64Const(UNDEFINED_BITS), I64Ne, I32And,
        If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f; End);
    target_desc(&mut f, l, key, ret, helpers);
    wasm!(f; LocalGet(l.r), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
    // Reported absent: the target property must be configurable and the
    // target extensible.
    wasm!(f;
        LocalGet(l.a), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz, If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f; End);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f; LocalGet(l.ext), I32Eqz, If(BlockType::Empty));
    invariant(&mut f);
    wasm!(f; End, I32Const(0), Return, End);
    target_extensible(&mut f, l, ret, helpers);
    // The result record follows the target's in the list.
    wasm!(f; LocalGet(l.r), LocalGet(l.list), Call(helpers.to_desc()));
    check(&mut f, ret);
    wasm!(f;
        LocalGet(l.list), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(l.b),
        LocalGet(l.a), I32Const(5), I32Shl, I32Add, LocalSet(d),
    );
    // CompletePropertyDescriptor: absent fields read false / undefined.
    record_desc(&mut f, d, 0);
    wasm!(f;
        LocalTee(l.c), I32Const(DESC_HAS_GET | DESC_HAS_SET), I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(l.c),
        I32Const(DESC_HAS_GET | DESC_HAS_SET | DESC_HAS_ENUMERABLE | DESC_HAS_CONFIGURABLE),
        I32Or,
        Else,
        LocalGet(l.c),
        I32Const(DESC_HAS_VALUE | DESC_HAS_WRITABLE | DESC_HAS_ENUMERABLE | DESC_HAS_CONFIGURABLE),
        I32Or,
        End,
        LocalSet(l.c),
        LocalGet(d), LocalGet(l.c), F64ConvertI32U, I64ReinterpretF64, I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(l.ext), LocalGet(d), LocalGet(l.b), I32Const(0), LocalGet(l.a), Select,
        Call(helpers.desc_compatible()), I32Eqz, If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f;
        End,
        LocalGet(l.c), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz, If(BlockType::Empty),
        // A non-configurable result: a present, non-configurable target
        // property, non-writable when the result is.
        LocalGet(l.a), I32Eqz,
        LocalGet(l.a), If(BlockType::Result(ValType::I32)),
        LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Const(0), I32Ne,
        Else, I32Const(0), End,
        I32Or, If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f;
        End,
        LocalGet(l.c), I32Const(DESC_HAS_WRITABLE | ATTR_WRITABLE), I32And, I32Const(DESC_HAS_WRITABLE), I32Eq,
        LocalGet(l.e), I32Const(ATTR_WRITABLE), I32And, I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f; End, End);
    for field in 0..4 {
        wasm!(f; LocalGet(out), LocalGet(d), I64Load(m64(ELEMS_DATA_OFFSET + 8 * field)));
        wasm!(f; Call(helpers.array_push), Drop);
    }
    wasm!(f; I32Const(1), End);
    finish_builder(f)
}

/// `__proxy_define_own(p: i32, key: i32, desc: i32, value: i64, get: i64,
/// set: i64) -> i32` (§10.5.6): the trap receives
/// `FromPropertyDescriptor(Desc)` (only the present fields); a falsish
/// result is [`DEFINE_REJECT`]; a true one is checked against the target's
/// property ([`DEFINE_OK`]).
pub(in crate::emit) fn emit_proxy_define_own(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I32;
    let (mut f, l, o) = builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE, RAW_I32, ROOT_JS, ROOT_JS, ROOT_JS],
        &[ROOT_BASE],
    );
    let (p, key, desc, value, get, set) = (0, 1, 2, 3, 4, 5);
    let invariant =
        |f: &mut FnBuilder<'_>| throw_key(f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    prologue(&mut f, l, p, "defineProperty", ret, strings, helpers);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(&mut f, l, |f| {
        wasm!(f;
            LocalGet(l.t), I32WrapI64, LocalGet(key), LocalGet(desc),
            LocalGet(value), LocalGet(get), LocalGet(set), Call(helpers.define_own()),
        );
    });
    // FromPropertyDescriptor(Desc).
    emit_alloc_const(&mut f, helpers, abi::OBJ_PREFIX_SIZE.cast_signed());
    wasm!(f; LocalSet(o));
    emit_object_prefix(
        &mut f,
        o,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    let boolean = |bit: i32| {
        move |f: &mut FnBuilder<'_>| {
            wasm!(f; I64Const(TRUE_BITS), I64Const(FALSE_BITS), LocalGet(desc), I32Const(bit), I32And, Select);
        }
    };
    let fields: [(&str, i32, Push<'_>); 6] = [
        ("value", DESC_HAS_VALUE, &|f| wasm!(f; LocalGet(value))),
        ("writable", DESC_HAS_WRITABLE, &boolean(ATTR_WRITABLE)),
        ("get", DESC_HAS_GET, &|f| wasm!(f; LocalGet(get))),
        ("set", DESC_HAS_SET, &|f| wasm!(f; LocalGet(set))),
        ("enumerable", DESC_HAS_ENUMERABLE, &boolean(ATTR_ENUMERABLE)),
        (
            "configurable",
            DESC_HAS_CONFIGURABLE,
            &boolean(ATTR_CONFIGURABLE),
        ),
    ];
    for (name, present, push) in fields {
        wasm!(f;
            LocalGet(desc), I32Const(present), I32And, If(BlockType::Empty),
            LocalGet(o), I32Const(static_key(strings, name)),
        );
        push(&mut f);
        wasm!(f; I32Const(ATTRS_DEFAULT), Call(helpers.define), End);
    }
    wasm!(f; LocalGet(o));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.x));
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(l.kj), CallArg::Val(l.x)],
        ret,
        helpers,
    );
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        // The list holds Desc's record, then the target's.
        I32Const(0), Call(helpers.new_array()), LocalSet(l.list),
    );
    push_desc(&mut f, l.list, |f| wasm!(f; LocalGet(desc)), helpers);
    for slot in [value, get, set] {
        wasm!(f; LocalGet(l.list), LocalGet(slot), Call(helpers.array_push), Drop);
    }
    wasm!(f; LocalGet(l.t), LocalGet(key), LocalGet(l.list), Call(helpers.own_prop()), LocalSet(l.a));
    check(&mut f, ret);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f;
        LocalGet(l.list), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalSet(l.b),
        // c = settingConfigFalse.
        LocalGet(desc), I32Const(DESC_HAS_CONFIGURABLE | ATTR_CONFIGURABLE), I32And,
        I32Const(DESC_HAS_CONFIGURABLE), I32Eq, LocalSet(l.c),
        LocalGet(l.a), I32Eqz,
        If(BlockType::Empty),
        LocalGet(l.ext), I32Eqz, LocalGet(l.c), I32Or, If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f; End, Else);
    record_desc(&mut f, l.b, 1);
    wasm!(f;
        LocalSet(l.e),
        LocalGet(l.ext), LocalGet(l.b), LocalGet(l.b), I32Const(RECORD), I32Add,
        Call(helpers.desc_compatible()), I32Eqz,
        LocalGet(l.c), LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Const(0), I32Ne, I32And,
        I32Or,
        // A non-configurable writable data property reported non-writable.
        LocalGet(l.e), I32Const(DESC_HAS_GET | ATTR_CONFIGURABLE | ATTR_WRITABLE), I32And,
        I32Const(ATTR_WRITABLE), I32Eq,
        LocalGet(desc), I32Const(DESC_HAS_WRITABLE | ATTR_WRITABLE), I32And, I32Const(DESC_HAS_WRITABLE), I32Eq,
        I32And, I32Or,
        If(BlockType::Empty),
    );
    invariant(&mut f);
    wasm!(f; End, End, I32Const(DEFINE_OK), End);
    finish_builder(f)
}

/// `__proxy_has(p: i32, key: i32) -> i32` (§10.5.7): a falsish result may
/// not hide a non-configurable property, nor any property of a
/// non-extensible target.
pub(in crate::emit) fn emit_proxy_has(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE, ROOT_BASE], &[]);
    let (p, key) = (0, 1);
    prologue(&mut f, l, p, "has", ret, strings, helpers);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(
        &mut f,
        l,
        |f| wasm!(f; LocalGet(key), LocalGet(l.t), Call(helpers.has)),
    );
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(l.kj)],
        ret,
        helpers,
    );
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; LocalTee(l.c), I32Eqz, If(BlockType::Empty));
    target_desc(&mut f, l, key, ret, helpers);
    wasm!(f;
        LocalGet(l.a), If(BlockType::Empty),
        LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz, If(BlockType::Empty),
    );
    throw_key(&mut f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f; LocalGet(l.ext), I32Eqz, If(BlockType::Empty));
    throw_key(&mut f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End, End, End, LocalGet(l.c), End);
    finish_builder(f)
}

/// Throw `PROXY_INVARIANT` for `key` when the target property described by
/// `l.a` / `l.e` / `l.b` is non-configurable and `value_check` (run for a
/// non-writable data property) or `accessor_check` (run for an accessor,
/// given the getter / setter slot) pushes nonzero.
fn check_fixed(
    f: &mut FnBuilder<'_>,
    l: L,
    key: u32,
    ret: Ret,
    value_check: impl FnOnce(&mut FnBuilder<'_>),
    accessor: (u64, impl FnOnce(&mut FnBuilder<'_>)),
    helpers: Helpers<'_>,
) {
    wasm!(f;
        LocalGet(l.a), If(BlockType::Empty),
        LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz, If(BlockType::Empty),
        LocalGet(l.e), I32Const(DESC_HAS_VALUE | ATTR_WRITABLE), I32And, I32Const(DESC_HAS_VALUE), I32Eq,
        If(BlockType::Empty),
    );
    record_field(f, l.b, 0, 1);
    wasm!(f; LocalSet(l.x));
    value_check(f);
    wasm!(f; If(BlockType::Empty));
    throw_key(f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End, End, LocalGet(l.e), I32Const(DESC_HAS_GET), I32And, If(BlockType::Empty));
    record_field(f, l.b, 0, accessor.0);
    wasm!(f; LocalSet(l.x));
    (accessor.1)(f);
    wasm!(f; If(BlockType::Empty));
    throw_key(f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End, End, End, End);
}

/// `__proxy_get(p: i32, key: i32, recv: i64) -> i64` (§10.5.8): over a
/// non-configurable target property the result must be the value of a
/// non-writable data property, and `undefined` for an accessor without a
/// getter.
pub(in crate::emit) fn emit_proxy_get(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I64;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE, ROOT_BASE, ROOT_JS], &[]);
    let (p, key, recv) = (0, 1, 2);
    prologue(&mut f, l, p, "get", ret, strings, helpers);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), LocalGet(key), LocalGet(recv), Call(helpers.super_get()));
    });
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(l.kj), CallArg::Val(recv)],
        ret,
        helpers,
    );
    target_desc(&mut f, l, key, ret, helpers);
    check_fixed(
        &mut f,
        l,
        key,
        ret,
        |f| {
            emit_same_value(f, l.r, l.x, helpers);
            wasm!(f; I32Eqz);
        },
        (2, |f: &mut FnBuilder<'_>| {
            wasm!(f;
                LocalGet(l.x), I64Const(UNDEFINED_BITS), I64Eq,
                LocalGet(l.r), I64Const(UNDEFINED_BITS), I64Ne, I32And,
            );
        }),
        helpers,
    );
    wasm!(f; LocalGet(l.r), End);
    finish_builder(f)
}

/// `__proxy_set(p: i32, key: i32, v: i64, recv: i64) -> i32` (§10.5.9):
/// `ToBoolean` of the trap result; a true one over a non-configurable target
/// property needs `v` to be the value of a non-writable data property, and
/// an accessor to have a setter.
pub(in crate::emit) fn emit_proxy_set(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE, ROOT_BASE, ROOT_JS, ROOT_JS], &[]);
    let (p, key, v, recv) = (0, 1, 2, 3);
    prologue(&mut f, l, p, "set", ret, strings, helpers);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), LocalGet(key), LocalGet(v), LocalGet(recv), Call(helpers.set_bool()));
    });
    call_trap(
        &mut f,
        l,
        &[
            CallArg::Val(l.t),
            CallArg::Val(l.kj),
            CallArg::Val(v),
            CallArg::Val(recv),
        ],
        ret,
        helpers,
    );
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; I32Eqz, If(BlockType::Empty), I32Const(0), Return, End);
    target_desc(&mut f, l, key, ret, helpers);
    check_fixed(
        &mut f,
        l,
        key,
        ret,
        |f| {
            emit_same_value(f, v, l.x, helpers);
            wasm!(f; I32Eqz);
        },
        (3, |f: &mut FnBuilder<'_>| {
            wasm!(f; LocalGet(l.x), I64Const(UNDEFINED_BITS), I64Eq);
        }),
        helpers,
    );
    wasm!(f; I32Const(1), End);
    finish_builder(f)
}

/// `__proxy_delete(p: i32, key: i32) -> i32` (§10.5.10): `ToBoolean` of the
/// trap result; a true one may not report a non-configurable property, nor
/// any property of a non-extensible target, as deleted.
pub(in crate::emit) fn emit_proxy_delete(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I32;
    let (mut f, l, _) = builder(leaves, &[ROOT_BASE, ROOT_BASE], &[]);
    let (p, key) = (0, 1);
    prologue(&mut f, l, p, "deleteProperty", ret, strings, helpers);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(l.kj));
    if_no_trap(&mut f, l, |f| {
        wasm!(f; LocalGet(l.t), LocalGet(key), I32Const(0), Call(helpers.delete));
    });
    call_trap(
        &mut f,
        l,
        &[CallArg::Val(l.t), CallArg::Val(l.kj)],
        ret,
        helpers,
    );
    to_boolean(&mut f, l.r, strings, helpers);
    wasm!(f; I32Eqz, If(BlockType::Empty), I32Const(0), Return, End);
    target_desc(&mut f, l, key, ret, helpers);
    wasm!(f;
        LocalGet(l.a), I32Eqz, If(BlockType::Empty), I32Const(1), Return, End,
        LocalGet(l.e), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz, If(BlockType::Empty),
    );
    throw_key(&mut f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End);
    target_extensible(&mut f, l, ret, helpers);
    wasm!(f; LocalGet(l.ext), I32Eqz, If(BlockType::Empty));
    throw_key(&mut f, key, PROP_THROW_PROXY_INVARIANT, ret, helpers);
    wasm!(f; End, I32Const(1), End);
    finish_builder(f)
}

/// The extra locals of `__proxy_own_keys`.
#[derive(Clone, Copy)]
struct Keys {
    /// The key list (rooted base).
    ks: u32,
    /// The target's keys (rooted base).
    tks: u32,
    /// The filtered result (rooted base).
    out: u32,
    /// Loop bound and index (raw).
    n: u32,
    i: u32,
    /// An interned key (rooted base).
    kb: u32,
}

/// Open a loop over the array in i32 `arr` with index `k.i` below `k.n`.
fn keys_loop(f: &mut FnBuilder<'_>, k: Keys, arr: u32) {
    wasm!(f;
        LocalGet(arr), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(k.n),
        I32Const(0), LocalSet(k.i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(k.i), LocalGet(k.n), I32GeU, BrIf(1),
    );
}

/// Close a [`keys_loop`].
fn keys_loop_end(f: &mut FnBuilder<'_>, k: Keys) {
    wasm!(f; LocalGet(k.i), I32Const(1), I32Add, LocalSet(k.i), Br(0), End, End);
}

/// Load element `k.i` of the array in i32 `arr` into `out`.
fn keys_load(f: &mut FnBuilder<'_>, k: Keys, arr: u32, out: u32) {
    emit_elem_addr(f, arr, k.i);
    wasm!(f; I64Load(m64(ELEMS_DATA_OFFSET)), LocalSet(out));
}

/// The target's full `[[OwnPropertyKeys]]` list into the rooted base
/// `into`: a `LAZY` function target is materialized first, and an array
/// target's `"length"` follows its integer keys (`__own_keys` lists
/// neither). Clobbers `k.out`, `k.n`, `k.i`, `l.a`, `l.c` and `l.x`.
fn target_keys(
    f: &mut FnBuilder<'_>,
    l: L,
    k: Keys,
    into: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let ret = Ret::I64;
    wasm!(f; LocalGet(l.t), I32WrapI64, LocalSet(l.a));
    emit_materialize(f, l.a, helpers);
    wasm!(f; LocalGet(l.t), I32Const(OWN_ALL_KEYS), Call(helpers.own_keys), I32WrapI64, LocalSet(into));
    check(f, ret);
    wasm!(f; LocalGet(l.t), I32WrapI64, I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq, If(BlockType::Empty));
    emit_with_array_length(f, [into, k.out, k.n, k.i, l.c], l.x, strings, helpers);
    wasm!(f; LocalGet(k.out), LocalSet(into), End);
}

/// `CreateListFromArrayLike(l.r, « String, Symbol »)` into a fresh `k.ks`:
/// a non-object, a non-string non-symbol element and a duplicate throw;
/// strings are interned.
fn trap_keys(f: &mut FnBuilder<'_>, l: L, k: Keys, strings: &StrOffsets, helpers: Helpers<'_>) {
    let ret = Ret::I64;
    let invariant = |f: &mut FnBuilder<'_>| {
        throw_name(
            f,
            "ownKeys",
            PROP_THROW_PROXY_INVARIANT,
            ret,
            strings,
            helpers,
        );
    };
    emit_is_object(f, l.r);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    invariant(f);
    wasm!(f;
        End,
        LocalGet(l.r), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get), LocalSet(l.x),
    );
    check(f, ret);
    wasm!(f; LocalGet(l.x), Call(helpers.to_number), LocalSet(l.x));
    check(f, ret);
    wasm!(f;
        LocalGet(l.x), F64ReinterpretI64, I32TruncSatF64U, LocalSet(k.n),
        I32Const(0), Call(helpers.new_array()), LocalSet(k.ks),
        I32Const(0), LocalSet(k.i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(k.i), LocalGet(k.n), I32GeU, BrIf(1),
        LocalGet(l.r),
        LocalGet(k.i), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
        Call(helpers.get), LocalSet(l.x),
    );
    check(f, ret);
    emit_is_string(f, l.x);
    emit_is_symbol(f, l.x);
    wasm!(f; I32Or, I32Eqz, If(BlockType::Empty));
    invariant(f);
    wasm!(f; End, LocalGet(l.x), I32Const(1), Call(helpers.to_key));
    emit_box_heap(f);
    wasm!(f; LocalSet(l.x));
    emit_array_contains(f, k.ks, l.x, l.a, l.e);
    wasm!(f; If(BlockType::Empty));
    invariant(f);
    wasm!(f; End, LocalGet(k.ks), LocalGet(l.x), Call(helpers.array_push), Drop);
    keys_loop_end(f, k);
}

/// The `[[OwnPropertyKeys]]` invariants of the trap list `k.ks` against
/// the target: every non-configurable target key, and every key of a
/// non-extensible target, must be listed; a non-extensible target lists
/// nothing else.
fn check_trap_keys(
    f: &mut FnBuilder<'_>,
    l: L,
    k: Keys,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let ret = Ret::I64;
    let invariant = |f: &mut FnBuilder<'_>| {
        throw_name(
            f,
            "ownKeys",
            PROP_THROW_PROXY_INVARIANT,
            ret,
            strings,
            helpers,
        );
    };
    target_extensible(f, l, ret, helpers);
    target_keys(f, l, k, k.tks, strings, helpers);
    wasm!(f; I32Const(0), Call(helpers.new_array()), LocalSet(l.list));
    keys_loop(f, k, k.tks);
    keys_load(f, k, k.tks, l.x);
    wasm!(f; LocalGet(l.x), I32Const(1), Call(helpers.to_key), LocalTee(k.kb));
    emit_box_heap(f);
    wasm!(f;
        LocalSet(l.x),
        LocalGet(l.t), LocalGet(k.kb), LocalGet(l.list), Call(helpers.own_prop()), LocalSet(l.a),
    );
    check(f, ret);
    wasm!(f; LocalGet(l.ext), I32Eqz, LocalGet(l.a), If(BlockType::Result(ValType::I32)));
    emit_last_record(f, l.list, l.b);
    record_desc(f, l.b, 0);
    wasm!(f;
        I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz,
        Else, I32Const(0), End,
        I32Or, If(BlockType::Empty),
    );
    emit_array_contains(f, k.ks, l.x, l.c, l.e);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    invariant(f);
    wasm!(f; End, End);
    keys_loop_end(f, k);
    wasm!(f;
        LocalGet(l.ext), I32Eqz,
        LocalGet(k.ks), I32Load(m32(ARRAY_LEN_OFFSET)), LocalGet(k.tks), I32Load(m32(ARRAY_LEN_OFFSET)),
        I32Ne, I32And, If(BlockType::Empty),
    );
    invariant(f);
    wasm!(f; End);
}

/// Filter the key list `k.ks` of the proxy `p` by the `__own_keys` `mode`
/// (every key is returned as is) into a fresh `k.out`, and return it.
fn filter_keys(f: &mut FnBuilder<'_>, l: L, k: Keys, [p, mode]: [u32; 2], helpers: Helpers<'_>) {
    let ret = Ret::I64;
    wasm!(f;
        LocalGet(mode), I32Const(OWN_ALL_KEYS), I32Eq, If(BlockType::Empty),
        LocalGet(k.ks),
    );
    emit_box_heap(f);
    wasm!(f;
        Return, End,
        I32Const(0), Call(helpers.new_array()), LocalSet(k.out),
        LocalGet(mode), I32Const(OWN_ALL), I32Eq, LocalGet(mode), I32Const(OWN_SYMBOLS), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    keys_loop(f, k, k.ks);
    keys_load(f, k, k.ks, l.x);
    emit_is_symbol(f, l.x);
    wasm!(f;
        LocalGet(mode), I32Const(OWN_SYMBOLS), I32Eq, I32Eq,
        If(BlockType::Empty), LocalGet(k.out), LocalGet(l.x), Call(helpers.array_push), Drop, End,
    );
    keys_loop_end(f, k);
    // EnumerableOwnProperties: per string key `[[GetOwnProperty]]`, then
    // for values and entries `[[Get]]`.
    wasm!(f; Else, I32Const(0), Call(helpers.new_array()), LocalSet(l.list));
    keys_loop(f, k, k.ks);
    keys_load(f, k, k.ks, l.x);
    emit_is_string(f, l.x);
    wasm!(f; If(BlockType::Empty), LocalGet(l.x), I32Const(1), Call(helpers.to_key), LocalTee(k.kb));
    emit_box_heap(f);
    wasm!(f;
        LocalSet(l.x),
        LocalGet(p), LocalGet(k.kb), LocalGet(l.list), Call(helpers.proxy_own_prop()), LocalSet(l.a),
    );
    check(f, ret);
    wasm!(f; LocalGet(l.a), If(BlockType::Empty));
    emit_last_record(f, l.list, l.b);
    record_desc(f, l.b, 0);
    wasm!(f;
        I32Const(ATTR_ENUMERABLE), I32And, If(BlockType::Empty),
        LocalGet(mode), I32Const(OWN_KEYS), I32Ne, If(BlockType::Empty),
        LocalGet(p),
    );
    emit_box_heap(f);
    wasm!(f; LocalGet(k.kb), Call(helpers.get), LocalSet(l.r));
    check(f, ret);
    wasm!(f;
        LocalGet(mode), I32Const(OWN_ENTRIES), I32Eq, If(BlockType::Empty),
        I32Const(2), Call(helpers.new_array()), LocalTee(l.c),
        I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalGet(l.x), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(l.c), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalGet(l.r), I64Store(m64(ELEMS_DATA_OFFSET + 8)),
        LocalGet(l.c),
    );
    emit_box_heap(f);
    wasm!(f;
        LocalSet(l.r),
        End,
        LocalGet(l.r), LocalSet(l.x),
        End,
        LocalGet(k.out), LocalGet(l.x), Call(helpers.array_push), Drop,
        End,
        End,
        End,
    );
    keys_loop_end(f, k);
    wasm!(f; End, LocalGet(k.out));
    emit_box_heap(f);
}

/// `__proxy_own_keys(p: i32, mode: i32) -> i64` (§10.5.11): the keys of
/// `[[OwnPropertyKeys]]` — the target's full list (`LAZY` keys materialized,
/// an array's `"length"` included) without a trap, else the trap result through `CreateListFromArrayLike`
/// (an object; strings and symbols only, strings interned; no duplicates)
/// checked against the target (every non-configurable key listed; a
/// non-extensible target's keys exactly) — then filtered by the
/// `__own_keys` `mode`: every key, the strings, the symbols, or
/// `EnumerableOwnProperties` (per string key one `[[GetOwnProperty]]` of
/// the proxy, then, for values and entries, its `[[Get]]`).
pub(in crate::emit) fn emit_proxy_own_keys(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let ret = Ret::I64;
    let (mut f, l, first) = builder(
        leaves,
        &[ROOT_BASE, RAW_I32],
        &[ROOT_BASE, ROOT_BASE, ROOT_BASE, RAW_I32, RAW_I32, ROOT_BASE],
    );
    let (p, mode) = (0, 1);
    let k = Keys {
        ks: first,
        tks: first + 1,
        out: first + 2,
        n: first + 3,
        i: first + 4,
        kb: first + 5,
    };
    prologue(&mut f, l, p, "ownKeys", ret, strings, helpers);
    wasm!(f;
        Block(BlockType::Empty),
        LocalGet(l.tr), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty),
    );
    target_keys(&mut f, l, k, k.ks, strings, helpers);
    wasm!(f; Br(1), End);
    call_trap(&mut f, l, &[CallArg::Val(l.t)], ret, helpers);
    trap_keys(&mut f, l, k, strings, helpers);
    check_trap_keys(&mut f, l, k, strings, helpers);
    wasm!(f; End);
    filter_keys(&mut f, l, k, [p, mode], helpers);
    wasm!(f; End);
    finish_builder(f)
}

impl Helpers<'_> {
    /// Whether the program can hold a proxy (the §6.11 reachability gate).
    pub(in crate::emit) fn proxies(self) -> bool {
        self.proxy_gate == ProxyGate::Open
    }

    /// `__proxy_trap(p, name) -> i64` (see [`emit_proxy_trap`]).
    pub(in crate::emit) fn proxy_trap(self) -> u32 {
        self.extra(registry::Extra::ProxyTrap)
    }

    /// `__get_proto(v) -> i64` (see [`emit_get_proto`]).
    pub(in crate::emit) fn get_proto(self) -> u32 {
        self.extra(registry::Extra::GetProto)
    }

    /// `__own_prop(obj, key, list) -> i32` (see [`emit_own_prop`]).
    pub(in crate::emit) fn own_prop(self) -> u32 {
        self.extra(registry::Extra::OwnProp)
    }

    /// `__desc_compatible(ext, d, cur) -> i32` (see [`emit_desc_compatible`]).
    pub(in crate::emit) fn desc_compatible(self) -> u32 {
        self.extra(registry::Extra::DescCompatible)
    }

    /// `__set_bool(target, key, v, recv) -> i32` (see [`emit_set_bool`]).
    pub(in crate::emit) fn set_bool(self) -> u32 {
        self.extra(registry::Extra::SetBool)
    }

    /// `__is_array(v) -> i32` (see [`emit_is_array`]).
    pub(in crate::emit) fn is_array(self) -> u32 {
        self.extra(registry::Extra::IsArray)
    }

    /// `__proxy_get_proto(p) -> i64` (see [`emit_proxy_get_proto`]).
    pub(in crate::emit) fn proxy_get_proto(self) -> u32 {
        self.extra(registry::Extra::ProxyGetProto)
    }

    /// `__proxy_set_proto(p, v) -> i32` (see [`emit_proxy_set_proto`]).
    pub(in crate::emit) fn proxy_set_proto(self) -> u32 {
        self.extra(registry::Extra::ProxySetProto)
    }

    /// `__proxy_is_extensible(p) -> i32` (see [`emit_proxy_is_extensible`]).
    pub(in crate::emit) fn proxy_is_extensible(self) -> u32 {
        self.extra(registry::Extra::ProxyIsExtensible)
    }

    /// `__proxy_prevent_ext(p) -> i32` (see [`emit_proxy_prevent_ext`]).
    pub(in crate::emit) fn proxy_prevent_ext(self) -> u32 {
        self.extra(registry::Extra::ProxyPreventExt)
    }

    /// `__proxy_own_prop(p, key, out) -> i32` (see [`emit_proxy_own_prop`]).
    pub(in crate::emit) fn proxy_own_prop(self) -> u32 {
        self.extra(registry::Extra::ProxyOwnProp)
    }

    /// `__proxy_define_own(p, key, desc, value, get, set) -> i32` (see
    /// [`emit_proxy_define_own`]).
    pub(in crate::emit) fn proxy_define_own(self) -> u32 {
        self.extra(registry::Extra::ProxyDefineOwn)
    }

    /// `__proxy_has(p, key) -> i32` (see [`emit_proxy_has`]).
    pub(in crate::emit) fn proxy_has(self) -> u32 {
        self.extra(registry::Extra::ProxyHas)
    }

    /// `__proxy_get(p, key, recv) -> i64` (see [`emit_proxy_get`]).
    pub(in crate::emit) fn proxy_get(self) -> u32 {
        self.extra(registry::Extra::ProxyGet)
    }

    /// `__proxy_set(p, key, v, recv) -> i32` (see [`emit_proxy_set`]).
    pub(in crate::emit) fn proxy_set(self) -> u32 {
        self.extra(registry::Extra::ProxySet)
    }

    /// `__proxy_delete(p, key) -> i32` (see [`emit_proxy_delete`]).
    pub(in crate::emit) fn proxy_delete(self) -> u32 {
        self.extra(registry::Extra::ProxyDelete)
    }

    /// `__proxy_own_keys(p, mode) -> i64` (see [`emit_proxy_own_keys`]).
    pub(in crate::emit) fn proxy_own_keys(self) -> u32 {
        self.extra(registry::Extra::ProxyOwnKeys)
    }
}

/// Emit `if (kind of the block in i32 `o` is a proxy) { body }` when the
/// program can hold proxies (DESIGN.md §6.11 reachability gate); nothing
/// otherwise.
pub(in crate::emit) fn emit_if_proxy<S: Sink>(
    function: &mut S,
    o: u32,
    helpers: Helpers<'_>,
    body: impl FnOnce(&mut S),
) {
    if helpers.proxies() {
        wasm!(function;
            LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_PROXY), I32Eq,
            If(BlockType::Empty),
        );
        body(function);
        wasm!(function; End);
    }
}

/// Set i32 `out` to the address of the last descriptor record appended to
/// the array in i32 `list` (fields at `out + ELEMS_DATA_OFFSET + 8 * k`).
pub(in crate::emit) fn emit_last_record(function: &mut impl Sink, list: u32, out: u32) {
    wasm!(function;
        LocalGet(list), I32Load(m32(ARRAY_ELEMS_OFFSET)),
        LocalGet(list), I32Load(m32(ARRAY_LEN_OFFSET)), I32Const(3), I32Shl, I32Add,
        I32Const(RECORD), I32Sub, LocalSet(out),
    );
}
