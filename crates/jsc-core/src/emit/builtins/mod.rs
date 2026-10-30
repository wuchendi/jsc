//! Builtin function bodies (DESIGN.md §6.4): one wasm function per
//! [`Builtin`], with the uniform function ABI `(a0..a_{K-1}, env, argc) -> i64`
//! so a builtin is called through the funcref table exactly like a user
//! function. `this` is read from `__this` on entry, the arguments from the
//! positional slots (a slot past the call's argument count holds `undefined`),
//! and a throw leaves the exception pending and returns, like any callee.
//!
//! Only the constructor bodies ([`ctor`]) and the bound-function trampoline
//! ([`Builtin::BoundCall`]) see the construct marker in `argc`.

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
mod async_iter;
mod ctor;
mod date;
mod error;
mod function;
mod generator;
mod global;
mod iter;
mod iterator;
mod json;
mod map_set;
mod namespace;
mod number;
mod object;
mod object_static;
mod promise;
mod string;
mod symbol;

use crate::emit::realm::{ArrayCopyOp, DateGenericOp};
pub(super) use async_iter::{
    emit_async_iterator_complete, emit_async_iterator_next, emit_async_iterator_return,
    emit_iterator_result_check, emit_open_async_iterator,
};
pub(super) use iter::{
    CloseMode, ST_ARRAY, ST_GEN, ST_STRING, emit_get_iterator, emit_iter_classify,
    emit_iterator_close, emit_iterator_step, emit_iterator_value, emit_open_iterator,
};
pub(super) use json::{emit_json_indent, emit_json_rep, emit_json_revive};
pub(super) use namespace::fragment;
use number::NumberFormat;
pub(super) use promise::emit_promise_self_error;
pub(super) use string::{emit_is_method_refusal, inline_host_op};

/// Argument `i` (an i32 local) into i64 `out`: element `i` of the spread
/// argument array captured (as the `__spread_args` jsval) in i64 `spread`
/// when the call passed more than `K` arguments, else slot `i` (`undefined`
/// past the slots); a spread hole reads `undefined`.
fn arg_at(function: &mut Function, frame: Frame, i: u32, spread: u32, out: u32) {
    let k = frame.k;
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U,
        I32Const(k.cast_signed()), I32GtU,
        If(BlockType::Result(ValType::I64)),
        LocalGet(spread), I32WrapI64,
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), Else);
    for j in 0..k {
        wasm!(function;
            LocalGet(i), I32Const(j.cast_signed()), I32Eq,
            If(BlockType::Result(ValType::I64)), LocalGet(j), Else,
        );
    }
    wasm!(function; I64Const(UNDEFINED_BITS));
    for _ in 0..k {
        wasm!(function; End);
    }
    wasm!(function; End, LocalSet(out));
    emit_elem_read(function, out);
    wasm!(function; LocalSet(out));
}

/// The IR fragments the hand-written bodies emit with `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let mut nodes = array::fragments(k);
    nodes.extend(function::fragments(k));
    nodes.extend(map_set::fragments(k));
    nodes.extend(promise::fragments(k));
    nodes.extend(generator::fragments(k));
    nodes.extend(generator::async_fragments(k));
    nodes.extend(date::fragments(k));
    nodes.extend(json::fragments());
    nodes.extend(iter::fragments(k));
    nodes.extend(async_iter::fragments(k));
    nodes.extend(iterator::fragments());
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
    /// Start a builtin body under the program's arity `K`: its wasm function
    /// and frame, with the unwind state reset.
    fn begin(helpers: Helpers<'_>) -> (Function, Self) {
        helpers.unwind.borrow_mut().reset();
        let frame = Self {
            k: helpers.max_args,
        };
        (Self::function(), frame)
    }

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
            (6, ValType::I64),
            (4, ValType::I32),
            (2, ValType::F64),
            (6, ValType::F64),
            (4, ValType::I64),
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

    /// Further i64 temp `n` (`0..6`), past the [`Self::arg`] temp.
    fn y64(self, n: u32) -> u32 {
        self.scratch() + 36 + n
    }

    /// Further i32 temp `n` (`0..4`).
    fn y32(self, n: u32) -> u32 {
        self.scratch() + 42 + n
    }

    /// Further f64 temp `n` (`0..2`).
    fn yf64(self, n: u32) -> u32 {
        self.scratch() + 46 + n
    }

    /// Further f64 temp `n` (`0..6`).
    fn zf64(self, n: u32) -> u32 {
        self.scratch() + 48 + n
    }

    /// Further i64 temp `n` (`0..4`): an iterator record's places.
    fn z64(self, n: u32) -> u32 {
        self.scratch() + 54 + n
    }
}

/// Every static string the builtin bodies reference. Interned in every program.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    object::STRINGS
        .into_iter()
        .chain(object_static::strings())
        .chain(function::STRINGS)
        .chain(error::STRINGS)
        .chain(array::strings())
        .chain(date::STRINGS)
        .chain(string::strings())
        .chain(number::STRINGS)
        .chain(number::SUM_STRINGS)
        .chain(map_set::strings())
        .chain(promise::strings())
        .chain(generator::strings())
        .chain(generator::async_strings())
        .chain(json::strings())
        .chain(symbol::STRINGS)
        .chain(iter::strings())
        .chain(async_iter::strings())
        .chain(iterator::STRINGS)
        .chain([TYPE_ERROR_STACK_PREFIX])
}

/// Emit the body of `builtin`.
pub(super) fn emit_builtin(
    builtin: Builtin,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) -> Function {
    let (mut f, frame) = Frame::begin(helpers);
    match builtin {
        Builtin::FunctionPrototype => wasm!(f; I64Const(UNDEFINED_BITS)),
        Builtin::HasOwnProperty => object::emit_has_own_property(&mut f, frame, strings, helpers),
        Builtin::ObjectToString => object::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::ValueOf => object::emit_value_of(&mut f, frame, strings, helpers),
        Builtin::IsPrototypeOf => object::emit_is_prototype_of(&mut f, frame, strings, helpers),
        Builtin::ToLocaleString => object::emit_to_locale_string(&mut f, frame, strings, helpers),
        Builtin::ProtoGet => object::emit_proto_get(&mut f, frame, strings, helpers),
        Builtin::ProtoSet => object::emit_proto_set(&mut f, frame, strings, helpers),
        Builtin::ObjectReflect(op) => object_static::emit(op, &mut f, frame, strings, helpers),
        Builtin::Call
        | Builtin::Apply
        | Builtin::Bind
        | Builtin::FunctionToString
        | Builtin::FunctionHasInstance
        | Builtin::BoundCall
        | Builtin::ThrowTypeError
        | Builtin::MethodCall => emit_function_family(builtin, &mut f, frame, strings, helpers),
        Builtin::ErrorToString => error::emit_to_string(&mut f, frame, strings, helpers),
        Builtin::ArrayPush
        | Builtin::ArrayPop
        | Builtin::ArrayHof(_)
        | Builtin::ArrayIndexOf
        | Builtin::ArrayIncludes
        | Builtin::ArrayJoin
        | Builtin::ArraySlice
        | Builtin::ArrayToString
        | Builtin::ArrayToLocaleString
        | Builtin::ArrayOf
        | Builtin::ArrayFrom
        | Builtin::ArrayAt
        | Builtin::ArrayFind(_)
        | Builtin::ArrayReduceRight
        | Builtin::ArrayLastIndexOf
        | Builtin::ArrayConcat
        | Builtin::ArrayFlat { .. }
        | Builtin::ArraySplice
        | Builtin::ArraySort
        | Builtin::ArrayReverse
        | Builtin::ArrayShift
        | Builtin::ArrayUnshift
        | Builtin::ArrayFill
        | Builtin::ArrayCopyWithin
        | Builtin::ArrayCopy(_) => emit_array_family(builtin, &mut f, frame, strings, helpers),
        Builtin::StringMethod(_)
        | Builtin::StringToString
        | Builtin::StringValueOf
        | Builtin::StringFromCharCode
        | Builtin::StringFromCodePoint
        | Builtin::StringRaw => emit_string_family(builtin, &mut f, frame, strings, helpers),
        Builtin::NumberToString
        | Builtin::NumberValueOf
        | Builtin::NumberToFixed
        | Builtin::NumberToExponential
        | Builtin::NumberToPrecision
        | Builtin::NumberToLocaleString
        | Builtin::NumberIs(_)
        | Builtin::ParseFloat
        | Builtin::ParseInt
        | Builtin::BooleanToString
        | Builtin::BooleanValueOf
        | Builtin::MathSumPrecise => emit_number_family(builtin, &mut f, frame, strings, helpers),
        Builtin::Protocol(op) => iter::emit_protocol(op, &mut f, frame, strings, helpers),
        Builtin::Iter(op) => iter::emit(op, &mut f, frame, strings, helpers),
        Builtin::PropertyIsEnumerable
        | Builtin::DefineAccessor { .. }
        | Builtin::LookupAccessor { .. }
        | Builtin::DateMethod(_)
        | Builtin::CollectionOp { .. }
        | Builtin::CollectionForEach { .. }
        | Builtin::CollectionSize { .. }
        | Builtin::Ctor(BuiltinCtor::Map | BuiltinCtor::Set | BuiltinCtor::Symbol)
        | Builtin::Iterator(_)
        | Builtin::PromiseThen
        | Builtin::PromiseCatch
        | Builtin::PromiseFinally
        | Builtin::PromiseStatic(_)
        | Builtin::GeneratorNext
        | Builtin::GeneratorReturn
        | Builtin::GeneratorThrow
        | Builtin::AsyncGeneratorMethod(_)
        | Builtin::AsyncGeneratorDrain
        | Builtin::AsyncGeneratorStep
        | Builtin::AsyncGeneratorReaction(_)
        | Builtin::Species(_)
        | Builtin::DateGeneric(_) => emit_object_family(builtin, &mut f, frame, strings, helpers),
        Builtin::JsonStringify => json::emit_stringify(&mut f, frame, strings, helpers),
        Builtin::JsonParse => json::emit_parse(&mut f, frame, strings, helpers),
        Builtin::IsNaN | Builtin::IsFinite => emit_global(builtin, &mut f, frame, helpers),
        Builtin::Symbol(op) => symbol::emit(op, &mut f, frame, strings, helpers),
        Builtin::Math(_)
        | Builtin::ConsoleLog
        | Builtin::Ctor(_)
        | Builtin::ObjectStatic(_)
        | Builtin::IsArray
        | Builtin::DateNow => emit_fragment(builtin, &mut f, frame, strings, helpers),
    }
    wasm!(f; End);
    f
}

/// Emit the body of the global `isNaN` / `isFinite` into `f`.
fn emit_global(builtin: Builtin, f: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    if builtin == Builtin::IsNaN {
        global::emit_is_nan(f, frame, helpers);
    } else {
        global::emit_is_finite(f, frame, helpers);
    }
}

/// Emit the IR [`fragment`] body of `builtin` into `f`.
fn emit_fragment(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let body = fragment(builtin, frame.k).unwrap_or(IrNode::Undefined);
    emit_node(&body, f, frame.scratch(), strings, helpers);
}

/// Emit the body of a `String` static or `%String.prototype%` method into `f`
/// (the variants [`emit_builtin`] routes here).
fn emit_string_family(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::StringMethod(op) => string::emit_method(op, f, frame, strings, helpers),
        Builtin::StringToString => string::emit_this_value(false, f, frame, strings, helpers),
        Builtin::StringValueOf => string::emit_this_value(true, f, frame, strings, helpers),
        Builtin::StringFromCharCode | Builtin::StringFromCodePoint => {
            let code_point = builtin == Builtin::StringFromCodePoint;
            string::emit_from_char_code(code_point, f, frame, strings, helpers);
        }
        Builtin::StringRaw => string::emit_raw(f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// Emit the body of an `Object.prototype` accessor-or-enumerability method,
/// of a `Date`, `Map` / `Set`, `Promise` or (async) generator prototype
/// builtin, of a `Promise` static, of a `@@species` builtin, or of `Symbol`
/// and the `Iterator` builtins into `f` (the variants [`emit_builtin`]
/// routes here).
fn emit_object_family(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::PropertyIsEnumerable => {
            object::emit_property_is_enumerable(f, frame, strings, helpers);
        }
        Builtin::DefineAccessor { set } => {
            object::emit_define_accessor(set, f, frame, strings, helpers);
        }
        Builtin::LookupAccessor { set } => {
            object::emit_lookup_accessor(set, f, frame, strings, helpers);
        }
        Builtin::DateMethod(op) => date::emit_method(op, f, frame, strings, helpers),
        Builtin::DateGeneric(DateGenericOp::ToJson) => {
            date::emit_to_json(f, frame, strings, helpers);
        }
        Builtin::DateGeneric(DateGenericOp::ToPrimitive) => {
            date::emit_to_primitive(f, frame, strings, helpers);
        }
        Builtin::CollectionOp { set, op } => map_set::emit_op(set, op, f, frame, strings, helpers),
        Builtin::CollectionForEach { set } => {
            map_set::emit_for_each(set, f, frame, strings, helpers);
        }
        Builtin::CollectionSize { set } => map_set::emit_size(set, f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::Map) => map_set::emit_ctor(false, f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::Set) => map_set::emit_ctor(true, f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::Symbol) => symbol::emit_ctor(f, frame, strings, helpers),
        Builtin::Iterator(op) => iterator::emit(op, f, frame, strings, helpers),
        Builtin::PromiseThen => promise::emit_then(false, f, frame, strings, helpers),
        Builtin::PromiseCatch => promise::emit_then(true, f, frame, strings, helpers),
        Builtin::PromiseFinally => promise::emit_finally(f, frame, strings, helpers),
        Builtin::PromiseStatic(op) => promise::emit_static(op, f, frame, strings, helpers),
        Builtin::GeneratorNext => generator::emit_next(f, frame, strings, helpers),
        Builtin::GeneratorReturn => generator::emit_return(f, frame, strings, helpers),
        Builtin::GeneratorThrow => generator::emit_throw(f, frame, strings, helpers),
        Builtin::AsyncGeneratorMethod(request) => {
            generator::emit_async_method(request, f, frame, strings, helpers);
        }
        Builtin::AsyncGeneratorDrain => generator::emit_async_drain(f, frame, strings, helpers),
        Builtin::AsyncGeneratorStep => generator::emit_async_step(f, frame, strings, helpers),
        Builtin::AsyncGeneratorReaction(reaction) => {
            generator::emit_async_reaction(reaction, f, frame, strings, helpers);
        }
        Builtin::Species(op) => promise::emit_species(op, f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// Emit the body of a `%Function.prototype%` method, the bound-function
/// trampoline, `%ThrowTypeError%` or the out-of-line method call into `f`
/// (the variants [`emit_builtin`] routes here).
fn emit_function_family(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::Call => function::emit_call(f, frame, strings, helpers),
        Builtin::Apply => function::emit_apply(f, frame, strings, helpers),
        Builtin::Bind => function::emit_bind(f, frame, strings, helpers),
        Builtin::FunctionToString => function::emit_to_string(f, frame, strings, helpers),
        Builtin::FunctionHasInstance => function::emit_has_instance(f, frame, strings, helpers),
        Builtin::BoundCall => function::emit_bound_call(f, frame, strings, helpers),
        Builtin::ThrowTypeError => function::emit_throw_type_error(f, frame, strings, helpers),
        Builtin::MethodCall => function::emit_method_call(f, frame, strings, helpers),
        _ => {}
    }
}

/// Emit the body of an `Array` static or `%Array.prototype%` method into `f`
/// (the variants [`emit_builtin`] routes here).
fn emit_array_family(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::ArrayPush => array::emit_push(f, frame, strings, helpers),
        Builtin::ArrayPop => array::emit_pop(f, frame, strings, helpers),
        Builtin::ArrayHof(op) => array::emit_hof(op, f, frame, strings, helpers),
        Builtin::ArrayIndexOf => array::emit_search(false, f, frame, strings, helpers),
        Builtin::ArrayIncludes => array::emit_search(true, f, frame, strings, helpers),
        Builtin::ArrayJoin => array::emit_join(f, frame, strings, helpers),
        Builtin::ArraySlice => array::emit_slice(f, frame, strings, helpers),
        Builtin::ArrayToString => array::emit_to_string(f, frame, strings, helpers),
        Builtin::ArrayToLocaleString => array::emit_to_locale_string(f, frame, strings, helpers),
        Builtin::ArrayOf => array::emit_of(f, frame, strings, helpers),
        Builtin::ArrayFrom => array::emit_from(f, frame, strings, helpers),
        Builtin::ArrayAt => array::emit_at(f, frame, strings, helpers),
        Builtin::ArrayFind(op) => array::emit_find(op, f, frame, strings, helpers),
        Builtin::ArrayReduceRight => array::emit_reduce_right(f, frame, strings, helpers),
        Builtin::ArrayLastIndexOf => array::emit_last_index_of(f, frame, strings, helpers),
        Builtin::ArrayConcat => array::emit_concat(f, frame, strings, helpers),
        Builtin::ArrayFlat { map } => array::emit_flat(map, f, frame, strings, helpers),
        Builtin::ArraySplice => array::emit_splice(f, frame, strings, helpers),
        Builtin::ArraySort => array::emit_sort(false, f, frame, strings, helpers),
        Builtin::ArrayReverse => array::emit_reverse(f, frame, strings, helpers),
        Builtin::ArrayShift => array::emit_shift(f, frame, strings, helpers),
        Builtin::ArrayUnshift => array::emit_unshift(f, frame, strings, helpers),
        Builtin::ArrayFill => array::emit_fill(f, frame, strings, helpers),
        Builtin::ArrayCopyWithin => array::emit_copy_within(f, frame, strings, helpers),
        Builtin::ArrayCopy(ArrayCopyOp::With) => array::emit_with(f, frame, strings, helpers),
        Builtin::ArrayCopy(ArrayCopyOp::ToReversed) => {
            array::emit_to_reversed(f, frame, strings, helpers);
        }
        Builtin::ArrayCopy(ArrayCopyOp::ToSorted) => {
            array::emit_sort(true, f, frame, strings, helpers);
        }
        Builtin::ArrayCopy(ArrayCopyOp::ToSpliced) => {
            array::emit_to_spliced(f, frame, strings, helpers);
        }
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// Emit the body of a `Number` / `Boolean` prototype method, a `Number`
/// static, a global number function or `Math.sumPrecise` into `f` (the
/// variants [`emit_builtin`] routes here).
fn emit_number_family(
    builtin: Builtin,
    f: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::NumberToString => number::emit_number_to_string(f, frame, strings, helpers),
        Builtin::NumberValueOf => number::emit_number_value_of(f, frame, strings, helpers),
        Builtin::NumberToFixed => {
            number::emit_number_format(NumberFormat::Fixed, f, frame, strings, helpers);
        }
        Builtin::NumberToExponential => {
            let format = NumberFormat::Exponential;
            number::emit_number_format(format, f, frame, strings, helpers);
        }
        Builtin::NumberToPrecision => {
            let format = NumberFormat::Precision;
            number::emit_number_format(format, f, frame, strings, helpers);
        }
        Builtin::NumberToLocaleString => {
            number::emit_number_to_locale_string(f, frame, strings, helpers);
        }
        Builtin::NumberIs(test) => global::emit_number_is(test, f, frame),
        Builtin::ParseFloat => global::emit_parse(false, f, frame, strings, helpers),
        Builtin::ParseInt => global::emit_parse(true, f, frame, strings, helpers),
        Builtin::BooleanToString => number::emit_boolean(false, f, frame, strings, helpers),
        Builtin::BooleanValueOf => number::emit_boolean(true, f, frame, strings, helpers),
        Builtin::MathSumPrecise => number::emit_sum_precise(f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// Throw a `TypeError` with `text` (`[message, stack]`) and return.
fn throw_type_error(
    function: &mut Function,
    frame: Frame,
    text: CalleeErrorText<'_>,
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
/// string, symbol, number or boolean becomes a fresh wrapper object (DESIGN.md
/// §5.2) over its realm prototype; any other value (an object, or `null`/`undefined`,
/// which callers reject first) is returned as is. A String wrapper's
/// `length` is defined as its own read-only, non-configurable property; its
/// indices are virtual (`__str_exotic_index`).
pub(super) fn emit_to_object(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32), (1, ValType::I64)]);
    let (v, obj, class, proto) = (0, 1, 2, 3);
    let wrap = |f: &mut Function, c: u8, proto_bits: i64| {
        wasm!(f;
            I32Const(i32::from(c)), LocalSet(class),
            I64Const(proto_bits), LocalSet(proto),
        );
    };
    let ctor_proto = |ctor| intrinsic_bits(realm::prototype_of(ctor), helpers);
    emit_is_string(&mut f, v);
    wasm!(f; If(BlockType::Empty));
    wrap(&mut f, abi::CLASS_STRING, ctor_proto(BuiltinCtor::String));
    wasm!(f; Else);
    emit_is_symbol(&mut f, v);
    wasm!(f; If(BlockType::Empty));
    wrap(
        &mut f,
        abi::CLASS_SYMBOL,
        intrinsic_bits(SYMBOL_PROTOTYPE, helpers),
    );
    wasm!(f; Else);
    emit_is_number(&mut f, v);
    wasm!(f; If(BlockType::Empty));
    wrap(&mut f, abi::CLASS_NUMBER, ctor_proto(BuiltinCtor::Number));
    wasm!(f;
        Else,
        LocalGet(v), I64Const(TRUE_BITS), I64Eq, LocalGet(v), I64Const(FALSE_BITS), I64Eq, I32Or,
        I32Eqz, If(BlockType::Empty), LocalGet(v), Return, End,
    );
    wrap(&mut f, abi::CLASS_BOOLEAN, ctor_proto(BuiltinCtor::Boolean));
    wasm!(f;
        End, End, End,
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
        LocalGet(obj), I32Const(static_key(strings, INDEX_LENGTH_KEY)),
        LocalGet(v), I32WrapI64,
        I32Load(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }),
        F64ConvertI32U, I64ReinterpretF64, I32Const(0), Call(helpers.define),
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
    strings: &StrOffsets,
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

/// Push the argument count (`argc`, a number jsval) as an i32.
fn push_argc(function: &mut Function, frame: Frame) {
    wasm!(function; LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U);
}

/// `while i < n { body; i += 1 }` over i32 locals.
fn count_loop(function: &mut Function, i: u32, n: u32, body: impl FnOnce(&mut Function)) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    body(function);
    wasm!(function;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
}

/// Push the heap jsval of the static string `text`.
fn push_str(function: &mut Function, strings: &StrOffsets, text: &str) {
    wasm!(function; I64Const(HEAP_BITS | i64::from(static_key(strings, text))));
}
