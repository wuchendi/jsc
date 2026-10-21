//! Builtin function bodies (design 4.1): one wasm function per
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
mod namespace;
mod object;

pub(super) use namespace::fragment;

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
        ])
    }

    /// Push argument `i` (`undefined` past the `K` slots).
    fn arg(self, function: &mut Function, i: u32) {
        if i < self.k {
            wasm!(function; LocalGet(i));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
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
}

/// Every static string the builtin bodies reference. Interned in every program.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    object::STRINGS
        .into_iter()
        .chain(function::STRINGS)
        .chain(error::STRINGS)
        .chain(array::STRINGS)
        .chain(date::STRINGS)
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
        Builtin::ErrorToString => error::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::ArrayMap => array::emit_map(&mut f, frame, strings, helpers),
        Builtin::ArrayToString => array::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::DateToString => date::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::DateValueOf => date::emit_value_of(&mut f, frame, strings, helpers),
        Builtin::Math(_)
        | Builtin::JsonStringify
        | Builtin::JsonParse
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
