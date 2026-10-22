//! Builtin function bodies (DESIGN.md §6.4): one wasm function per
//! [`Builtin`], with the uniform function ABI `(a0..a_{K-1}, env, argc) -> i64`
//! so a builtin is called through the funcref table exactly like a user
//! function. `this` is read from `__this` on entry, the arguments from the
//! positional slots (a slot past the call's argument count holds `undefined`),
//! and a throw leaves the exception pending and returns, like any callee.
//!
//! Only the constructor bodies ([`ctor`]) see the construct marker in `argc`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

mod array;
mod ctor;
mod date;
mod error;
mod function;
mod generator;
mod global;
mod iter;
mod json;
mod map_set;
mod namespace;
mod number;
mod object;
mod promise;
mod string;

pub(super) use json::{emit_json_indent, emit_json_rep, emit_json_revive};
pub(super) use namespace::fragment;
use number::NumberFormat;
pub(super) use string::host_ready_args;

/// The IR fragments the hand-written bodies emit with `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let mut nodes = array::fragments(k);
    nodes.extend(map_set::fragments(k));
    nodes.extend(promise::fragments(k));
    nodes.extend(generator::fragments(k));
    nodes.extend(date::fragments(k));
    nodes.extend(json::fragments());
    nodes
}

/// The locals of a builtin body: the `K` argument slots, `env` and `argc`
/// (the parameters), four i64 and four i32 temps, then the scratch layout
/// [`emit_function`] declares, so IR fragments (the thrown errors) can be
/// emitted with [`emit_node`].
#[derive(Clone, Copy)]
struct Frame {
    /// The uniform ABI's positional arity `K`.
    k: u32,
}

impl Frame {
    /// The wasm function with a frame's locals.
    fn function() -> Function {
        Function::new(vec![
            (4, ValType::I64),
            (4, ValType::I32),
            (3, ValType::I64),
            (1, ValType::I32),
            (3, ValType::I64),
            (1, ValType::I32),
            (5, ValType::I32),
            (1, ValType::I64),
            (7, ValType::I64),
            (1, ValType::I32),
            (2, ValType::I64),
            (6, ValType::I64),
            (2, ValType::I64),
            (2, ValType::I32),
            (1, ValType::F64),
            (1, ValType::I64),
        ])
    }

    /// Push argument `i`: slot `i`, or past the `K` slots the element of the
    /// spread argument array a call with more than `K` arguments published
    /// (`undefined` past the argument count, or for a spread hole).
    fn arg(self, function: &mut Function, i: u32) {
        if i < self.k {
            wasm!(function; LocalGet(i));
            return;
        }
        let tmp = self.scratch() + 35;
        wasm!(function;
            LocalGet(self.argc()), F64ReinterpretI64, F64Const(Ieee64::from(f64::from(i))), F64Gt,
            If(BlockType::Result(ValType::I64)),
            GlobalGet(SPREAD_ARGS_GLOBAL), I32WrapI64,
        );
        emit_array_elems(function);
        wasm!(function; I64Load(elem_slot(u64::from(i))), LocalSet(tmp));
        emit_elem_read(function, tmp);
        wasm!(function; Else, I64Const(UNDEFINED_BITS), End);
    }

    /// The `argc` parameter (a number jsval).
    fn argc(self) -> u32 {
        self.k + 1
    }

    /// i64 temp `n` (`0..4`).
    fn i64(self, n: u32) -> u32 {
        self.k + 2 + n
    }

    /// i32 temp `n` (`0..4`).
    fn i32(self, n: u32) -> u32 {
        self.k + 6 + n
    }

    /// The first scratch local of the [`emit_function`] layout.
    fn scratch(self) -> u32 {
        self.k + 10
    }

    /// Extra i64 temp `n` (`0..2`), past the scratch layout.
    fn x64(self, n: u32) -> u32 {
        self.scratch() + 30 + n
    }

    /// Extra i32 temp `n` (`0..2`), past the scratch layout.
    fn x32(self, n: u32) -> u32 {
        self.scratch() + 32 + n
    }

    /// The f64 temp `n` (`0`), past the scratch layout.
    fn f64(self, n: u32) -> u32 {
        self.scratch() + 34 + n
    }
}

/// Every static string the builtin bodies reference. Interned in every program.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    object::STRINGS
        .into_iter()
        .chain(function::STRINGS)
        .chain(error::STRINGS)
        .chain(array::strings())
        .chain(date::STRINGS)
        .chain(string::strings())
        .chain(number::STRINGS)
        .chain(map_set::strings())
        .chain(promise::strings())
        .chain(generator::strings())
        .chain(json::strings())
        .chain([TYPE_ERROR_STACK_PREFIX])
}

/// Emit the body of `builtin`.
pub(super) fn emit_builtin(
    builtin: Builtin,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let frame = Frame {
        k: helpers.max_args,
    };
    let mut f = Frame::function();
    helpers.unwind.borrow_mut().reset();
    match builtin {
        Builtin::FunctionPrototype => wasm!(f; I64Const(UNDEFINED_BITS)),
        Builtin::HasOwnProperty => object::emit_has_own_property(&mut f, frame, strings, helpers),
        Builtin::ObjectToString => object::emit_to_string(&mut f, frame, strings),
        Builtin::ValueOf => object::emit_value_of(&mut f, frame, strings, helpers),
        Builtin::Call => function::emit_call(&mut f, frame, strings, helpers),
        Builtin::Apply => function::emit_apply(&mut f, frame, strings, helpers),
        Builtin::ThrowTypeError => function::emit_throw_type_error(&mut f, frame, strings, helpers),
        Builtin::ErrorToString => error::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::ArrayPush => array::emit_push(&mut f, frame, strings, helpers),
        Builtin::ArrayPop => array::emit_pop(&mut f, frame, strings, helpers),
        Builtin::ArrayHof(op) => array::emit_hof(op, &mut f, frame, strings, helpers),
        Builtin::ArrayIndexOf => array::emit_search(false, &mut f, frame, strings, helpers),
        Builtin::ArrayIncludes => array::emit_search(true, &mut f, frame, strings, helpers),
        Builtin::ArrayJoin => array::emit_join(&mut f, frame, strings, helpers),
        Builtin::ArraySlice => array::emit_slice(&mut f, frame, strings, helpers),
        Builtin::ArrayToString => array::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::StringMethod(op) => string::emit_method(op, &mut f, frame, strings, helpers),
        Builtin::StringToString => string::emit_this_value(false, &mut f, frame, strings, helpers),
        Builtin::StringValueOf => string::emit_this_value(true, &mut f, frame, strings, helpers),
        Builtin::NumberToString => number::emit_number_to_string(&mut f, frame, strings, helpers),
        Builtin::NumberValueOf => number::emit_number_value_of(&mut f, frame, strings, helpers),
        Builtin::NumberToFixed => {
            number::emit_number_format(NumberFormat::ToFixed, &mut f, frame, strings, helpers);
        }
        Builtin::NumberToExponential => {
            let format = NumberFormat::ToExponential;
            number::emit_number_format(format, &mut f, frame, strings, helpers);
        }
        Builtin::NumberToPrecision => {
            let format = NumberFormat::ToPrecision;
            number::emit_number_format(format, &mut f, frame, strings, helpers);
        }
        Builtin::NumberToLocaleString => {
            number::emit_number_to_locale_string(&mut f, frame, strings, helpers);
        }
        Builtin::NumberIs(test) => global::emit_number_is(test, &mut f, frame),
        Builtin::ParseFloat => global::emit_parse(false, &mut f, frame, strings, helpers),
        Builtin::ParseInt => global::emit_parse(true, &mut f, frame, strings, helpers),
        Builtin::BooleanToString => number::emit_boolean(false, &mut f, frame, strings, helpers),
        Builtin::BooleanValueOf => number::emit_boolean(true, &mut f, frame, strings, helpers),
        Builtin::MethodCall => function::emit_method_call(&mut f, frame, strings, helpers),
        Builtin::IterSource => iter::emit_iter_source(&mut f, frame, strings, helpers),
        Builtin::DateMethod(op) => date::emit_method(op, &mut f, frame, strings, helpers),
        Builtin::DateToJson => date::emit_to_json(&mut f, frame, strings, helpers),
        Builtin::CollectionOp { set, op } => {
            map_set::emit_op(set, op, &mut f, frame, strings, helpers);
        }
        Builtin::CollectionForEach { set } => {
            map_set::emit_for_each(set, &mut f, frame, strings, helpers);
        }
        Builtin::CollectionSize { set } => map_set::emit_size(set, &mut f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::Map) => {
            map_set::emit_ctor(false, &mut f, frame, strings, helpers);
        }
        Builtin::Ctor(BuiltinCtor::Set) => {
            map_set::emit_ctor(true, &mut f, frame, strings, helpers);
        }
        Builtin::PromiseThen => promise::emit_then(false, &mut f, frame, strings, helpers),
        Builtin::PromiseCatch => promise::emit_then(true, &mut f, frame, strings, helpers),
        Builtin::PromiseFinally => promise::emit_finally(&mut f, frame, strings, helpers),
        Builtin::GeneratorNext => generator::emit_next(&mut f, frame, strings, helpers),
        Builtin::GeneratorReturn => generator::emit_return(&mut f, frame, strings, helpers),
        Builtin::GeneratorThrow => generator::emit_throw(&mut f, frame, strings, helpers),
        Builtin::JsonStringify => json::emit_stringify(&mut f, frame, strings, helpers),
        Builtin::JsonParse => json::emit_parse(&mut f, frame, strings, helpers),
        Builtin::IsNaN => global::emit_is_nan(&mut f, frame, helpers),
        Builtin::IsFinite => global::emit_is_finite(&mut f, frame, helpers),
        Builtin::Math(_)
        | Builtin::ConsoleLog
        | Builtin::Ctor(_)
        | Builtin::ObjectStatic(_)
        | Builtin::IsArray
        | Builtin::PromiseResolve
        | Builtin::PromiseReject
        | Builtin::DateNow => {
            let body = fragment(builtin, frame.k).unwrap_or(IrNode::Undefined);
            emit_node(&body, &mut f, frame.scratch(), strings, helpers);
        }
    }
    wasm!(f; End);
    f
}

/// Throw a `TypeError` with `text` (`[message, stack]`) and return.
fn throw_type_error(
    function: &mut Function,
    frame: Frame,
    text: CalleeErrorText<'_>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_throw_not_a_function(function, text, frame.scratch(), strings, helpers);
}

/// Push `1` when the jsval in `local` is a wrapper object of `class`
/// (DESIGN.md §5.2).
pub(super) fn emit_is_wrapper(function: &mut Function, local: u32, class: u8) {
    emit_is_pointer(function, local);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64, I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }),
        I32Const(0xff00_00ff_u32.cast_signed()), I32And,
        I32Const(TYPE_OBJECT | (i32::from(class) << 24)), I32Eq,
        Else, I32Const(0), End,
    );
}

/// `this<Type>Value(this)` into i64 `out`: the primitive itself (`is_prim`),
/// the `[[PrimitiveValue]]` of a wrapper object of `class` (`+0` for
/// `%Number.prototype%`, itself a Number object), else throw `text`.
#[allow(clippy::too_many_arguments)]
fn this_primitive(
    function: &mut Function,
    frame: Frame,
    out: u32,
    class: u8,
    is_prim: fn(&mut Function, u32),
    text: CalleeErrorText<'static>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(out));
    is_prim(function, out);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    let number_proto = class == abi::CLASS_NUMBER;
    if number_proto {
        let proto = intrinsic_bits(realm::prototype_of(BuiltinCtor::Number), helpers);
        wasm!(function;
            LocalGet(out), I64Const(proto), I64Eq,
            If(BlockType::Empty), I64Const(0), LocalSet(out), Else,
        );
    }
    emit_is_wrapper(function, out, class);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(out), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::WRAPPER_VALUE_OFFSET), align: 3, memory_index: 0 }),
        LocalSet(out),
        Else,
    );
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End, End);
    if number_proto {
        wasm!(function; End);
    }
}

/// `__to_object(v: i64) -> i64`: `ToObject` of a non-nullish jsval: a
/// string, number or boolean becomes a fresh wrapper object (DESIGN.md §5.2) over
/// its realm prototype; any other value (an object, or `null`/`undefined`,
/// which callers reject first) is returned as is. A String wrapper's
/// `length` and indices are defined as its own read-only, non-configurable
/// properties (the indices enumerable), so every property operation sees them.
pub(super) fn emit_to_object(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![
        (2, ValType::I32),
        (1, ValType::I64),
        (3, ValType::I32),
    ]);
    let (v, obj, class, proto, len, idx, string) = (0, 1, 2, 3, 4, 5, 6);
    let wrap = |f: &mut Function, c: u8, ctor: BuiltinCtor| {
        wasm!(f;
            I32Const(i32::from(c)), LocalSet(class),
            I64Const(intrinsic_bits(realm::prototype_of(ctor), helpers)), LocalSet(proto),
        );
    };
    emit_is_string(&mut f, v);
    wasm!(f; If(BlockType::Empty));
    wrap(&mut f, abi::CLASS_STRING, BuiltinCtor::String);
    wasm!(f; Else);
    emit_is_number(&mut f, v);
    wasm!(f; If(BlockType::Empty));
    wrap(&mut f, abi::CLASS_NUMBER, BuiltinCtor::Number);
    wasm!(f;
        Else,
        LocalGet(v), I64Const(TRUE_BITS), I64Eq, LocalGet(v), I64Const(FALSE_BITS), I64Eq, I32Or,
        I32Eqz, If(BlockType::Empty), LocalGet(v), Return, End,
    );
    wrap(&mut f, abi::CLASS_BOOLEAN, BuiltinCtor::Boolean);
    wasm!(f;
        End, End,
        I32Const(abi::WRAPPER_BLOCK_SIZE.cast_signed()), Call(helpers.alloc), LocalTee(obj),
        LocalGet(class), I32Const(24), I32Shl, I32Const(TYPE_OBJECT), I32Or,
        I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(obj), I32Const(0),
        I32Store(MemArg { offset: u64::from(abi::OBJ_PROPS_OFFSET), align: 2, memory_index: 0 }),
        LocalGet(obj), LocalGet(proto),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(obj), LocalGet(v),
        I64Store(MemArg { offset: u64::from(abi::WRAPPER_VALUE_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(class), I32Const(i32::from(abi::CLASS_STRING)), I32Eq,
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, LocalTee(string),
        I32Load(MemArg { offset: u64::from(abi::HEADER_SIZE), align: 2, memory_index: 0 }), LocalSet(len),
        I32Const(0), LocalSet(idx),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(idx), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(obj),
        LocalGet(idx), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
        LocalGet(string), LocalGet(idx), Call(helpers.str_index), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        I32Const(abi::ATTR_ENUMERABLE.cast_signed()), Call(helpers.define),
        LocalGet(idx), I32Const(1), I32Add, LocalSet(idx),
        Br(0),
        End, End,
        LocalGet(obj), I32Const(static_key(strings, INDEX_LENGTH_KEY)),
        LocalGet(len), F64ConvertI32U, I64ReinterpretF64, I32Const(0), Call(helpers.define),
        End,
        LocalGet(obj), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        End,
    );
    f
}

/// Push the target's environment, then (after `argc`, pushed by `argc`) its
/// funcref slot, and `call_indirect` it: the tail every dispatch shares.
fn invoke(
    function: &mut Function,
    target: u32,
    argc: impl FnOnce(&mut Function),
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(target), I32WrapI64,
        I64Load(MemArg { offset: FUNC_ENV_OFFSET, align: 3, memory_index: 0 }),
    );
    argc(function);
    wasm!(function;
        LocalGet(target), I32WrapI64,
        I32Load(MemArg { offset: FUNC_INDEX_OFFSET, align: 2, memory_index: 0 }),
        CallIndirect { type_index: helpers.fn_type, table_index: 0 },
    );
}

/// Emit `node` in the body's scratch layout, dropping its value.
fn emit_drop(
    function: &mut Function,
    frame: Frame,
    node: &IrNode,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(node, function, frame.scratch(), strings, helpers);
    wasm!(function; Drop);
}

/// Return `undefined` when the last call left an exception pending.
fn return_if_pending(function: &mut Function) {
    wasm!(function;
        GlobalGet(EXC_PENDING_GLOBAL),
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
}

/// Push the heap jsval of the static string `text`.
fn push_str(function: &mut Function, strings: &HashMap<String, i32>, text: &str) {
    wasm!(function; I64Const(HEAP_BITS | i64::from(static_key(strings, text))));
}
