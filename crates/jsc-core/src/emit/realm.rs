//! The realm image (DESIGN.md §6.4): every intrinsic object is a static block
//! in the data segment, laid out by the compiler from ONE registry of
//! `(intrinsic, key, attrs, value)` entries, so every intrinsic address is a
//! compile-time constant and costs nothing at startup.
//!
//! The image holds, per [`Intrinsic`] in [`intrinsics`] order, its
//! object-family block (a `TYPE_FUNCTION` block for a callable intrinsic)
//! directly followed by a full `TYPE_PROPS` table of its registry entries, and
//! then one static `TYPE_FUNCTION` block per builtin method ([`Value::Method`]):
//! `LAZY`, no properties, `proto = %Function.prototype%`, `env = undefined`,
//! and the funcref-table slot of its uniform-ABI wasm body
//! ([`super::builtins`]), and finally one static `TYPE_ACCESSOR` block per
//! [`Value::Accessor`] entry, holding its getter/setter method blocks. A
//! static table that a user mutation overflows is rebuilt on the heap by
//! `__define` like any other.

#[allow(clippy::wildcard_imports)]
use super::*;

/// An intrinsic object of the realm image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Intrinsic {
    /// `%Object.prototype%` (`proto` `null`).
    ObjectPrototype,
    /// `%Function.prototype%`: itself a function (it accepts any arguments
    /// and returns `undefined`).
    FunctionPrototype,
    /// `%<Kind>.prototype%` of a builtin error kind.
    ErrorPrototype(ErrorKind),
    /// The global object (`class GLOBAL`).
    Global,
    /// The `Math` namespace object.
    Math,
    /// The `JSON` namespace object.
    Json,
    /// The `console` namespace object.
    Console,
    /// A builtin constructor function.
    Ctor(BuiltinCtor),
    /// `%<Ctor>.prototype%` of a constructor without a dedicated variant
    /// (`Array`, `String`, `Number`, `Boolean`, `Map`, `Set`, `Promise`,
    /// `Date`): `constructor` and the prototype methods.
    Prototype(BuiltinCtor),
    /// `%GeneratorPrototype%`: `next`, `return`, `throw`; every generator
    /// object's `proto`.
    GeneratorPrototype,
}

/// The constructors whose prototype is an [`Intrinsic::Prototype`].
const PLAIN_PROTOTYPES: [BuiltinCtor; 8] = [
    BuiltinCtor::Array,
    BuiltinCtor::String,
    BuiltinCtor::Number,
    BuiltinCtor::Boolean,
    BuiltinCtor::Map,
    BuiltinCtor::Set,
    BuiltinCtor::Promise,
    BuiltinCtor::Date,
];

/// The `prototype` intrinsic of `ctor`.
pub(super) fn prototype_of(ctor: BuiltinCtor) -> Intrinsic {
    match ctor {
        BuiltinCtor::Object => Intrinsic::ObjectPrototype,
        BuiltinCtor::Function => Intrinsic::FunctionPrototype,
        BuiltinCtor::Error(kind) => Intrinsic::ErrorPrototype(kind),
        _ => Intrinsic::Prototype(ctor),
    }
}

impl From<GlobalIntrinsic> for Intrinsic {
    fn from(global: GlobalIntrinsic) -> Self {
        match global {
            GlobalIntrinsic::Global => Self::Global,
            GlobalIntrinsic::Math => Self::Math,
            GlobalIntrinsic::Json => Self::Json,
            GlobalIntrinsic::Console => Self::Console,
            GlobalIntrinsic::Ctor(ctor) => Self::Ctor(ctor),
        }
    }
}

impl Intrinsic {
    /// The intrinsic's `proto` (`None` = `null`).
    fn proto(self) -> Option<Intrinsic> {
        match self {
            Self::ObjectPrototype => None,
            Self::FunctionPrototype
            | Self::ErrorPrototype(ErrorKind::Error)
            | Self::Global
            | Self::Math
            | Self::Json
            | Self::Console
            | Self::Prototype(_)
            | Self::GeneratorPrototype => Some(Self::ObjectPrototype),
            Self::ErrorPrototype(_) => Some(Self::ErrorPrototype(ErrorKind::Error)),
            // The native error constructors inherit from `Error`.
            Self::Ctor(BuiltinCtor::Error(kind)) if kind != ErrorKind::Error => {
                Some(Self::Ctor(BuiltinCtor::Error(ErrorKind::Error)))
            }
            Self::Ctor(_) => Some(Self::FunctionPrototype),
        }
    }

    /// The builtin body a callable intrinsic runs, or `None` for an ordinary
    /// object.
    fn call(self) -> Option<Builtin> {
        match self {
            Self::FunctionPrototype => Some(Builtin::FunctionPrototype),
            Self::Ctor(ctor) => Some(Builtin::Ctor(ctor)),
            _ => None,
        }
    }

    /// The header `flags` of a callable intrinsic: a constructor is
    /// `CONSTRUCTOR`, and `SELF_ALLOC` when it allocates its own kind of
    /// object from `new.target` (DESIGN.md §7.6, §7.7).
    fn flags(self) -> u8 {
        match self {
            Self::Ctor(
                BuiltinCtor::Array
                | BuiltinCtor::Error(_)
                | BuiltinCtor::Map
                | BuiltinCtor::Set
                | BuiltinCtor::Promise
                | BuiltinCtor::Date,
            ) => abi::FUNC_FLAG_CONSTRUCTOR | abi::FUNC_FLAG_SELF_ALLOC,
            Self::Ctor(_) => abi::FUNC_FLAG_CONSTRUCTOR,
            _ => 0,
        }
    }

    /// The dirty-bit index of a watched intrinsic (DESIGN.md §6.4), or `None`.
    pub(super) fn watch_bit(self) -> Option<u8> {
        Some(match self {
            Self::ObjectPrototype => 0,
            Self::Prototype(BuiltinCtor::Array) => 1,
            Self::Prototype(BuiltinCtor::String) => 2,
            Self::Prototype(BuiltinCtor::Number) => 3,
            Self::Prototype(BuiltinCtor::Boolean) => 4,
            Self::FunctionPrototype => 5,
            Self::Prototype(BuiltinCtor::Promise) => 6,
            Self::Prototype(BuiltinCtor::Map) => 7,
            Self::Prototype(BuiltinCtor::Set) => 8,
            Self::Prototype(BuiltinCtor::Date) => 9,
            Self::Math => 10,
            Self::Json => 11,
            Self::Console => 12,
            Self::GeneratorPrototype => 13,
            _ => return None,
        })
    }

    /// The `TYPE_OBJECT` class byte of a non-callable intrinsic.
    fn class(self) -> u8 {
        if self == Self::Global {
            abi::CLASS_GLOBAL
        } else {
            abi::CLASS_ORDINARY
        }
    }
}

/// Every intrinsic, in image order.
pub(super) fn intrinsics() -> impl Iterator<Item = Intrinsic> {
    [Intrinsic::ObjectPrototype, Intrinsic::FunctionPrototype]
        .into_iter()
        .chain(ErrorKind::ALL.map(Intrinsic::ErrorPrototype))
        .chain([
            Intrinsic::Global,
            Intrinsic::Math,
            Intrinsic::Json,
            Intrinsic::Console,
        ])
        .chain(BuiltinCtor::ALL.map(Intrinsic::Ctor))
        .chain(PLAIN_PROTOTYPES.map(Intrinsic::Prototype))
        .chain([Intrinsic::GeneratorPrototype])
}

/// A builtin function body with the uniform ABI (see [`super::builtins`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Builtin {
    /// `%Function.prototype%` itself: returns `undefined`.
    FunctionPrototype,
    /// `Object.prototype.hasOwnProperty`.
    HasOwnProperty,
    /// `Object.prototype.toString`.
    ObjectToString,
    /// `Object.prototype.valueOf`.
    ValueOf,
    /// `Function.prototype.call`.
    Call,
    /// `Function.prototype.apply`.
    Apply,
    /// `Error.prototype.toString`.
    ErrorToString,
    /// A `Math` method (`Math.floor`, `Math.max`, ..).
    Math(MathOp),
    /// `JSON.stringify`.
    JsonStringify,
    /// `JSON.parse`.
    JsonParse,
    /// `console.log`.
    ConsoleLog,
    /// A constructor's body (called or constructed).
    Ctor(BuiltinCtor),
    /// `Object.keys` / `Object.values` / `Object.entries`.
    ObjectStatic(ObjectBuiltinOp),
    /// `Array.isArray`.
    IsArray,
    /// `Promise.resolve`.
    PromiseResolve,
    /// `Promise.reject`.
    PromiseReject,
    /// `Date.now`.
    DateNow,
    /// `Array.prototype.push`.
    ArrayPush,
    /// `Array.prototype.pop`.
    ArrayPop,
    /// `Array.prototype.map` / `filter` / `reduce` / `forEach`.
    ArrayHof(ArrayHofOp),
    /// `Array.prototype.indexOf`.
    ArrayIndexOf,
    /// `Array.prototype.includes`.
    ArrayIncludes,
    /// `Array.prototype.join`.
    ArrayJoin,
    /// `Array.prototype.slice`.
    ArraySlice,
    /// `Array.prototype.toString`.
    ArrayToString,
    /// A `String.prototype` method backed by a host `__method` op.
    StringMethod(StringOp),
    /// `String.prototype.toString`.
    StringToString,
    /// `String.prototype.valueOf`.
    StringValueOf,
    /// `Number.prototype.toString`.
    NumberToString,
    /// `Number.prototype.valueOf`.
    NumberValueOf,
    /// `Number.prototype.toFixed`.
    NumberToFixed,
    /// `Number.prototype.toExponential`.
    NumberToExponential,
    /// `Number.prototype.toPrecision`.
    NumberToPrecision,
    /// `Number.prototype.toLocaleString`: `toString()` (no locale data).
    NumberToLocaleString,
    /// A `Number` predicate static (`Number.isInteger`, ..): no coercion.
    NumberIs(NumberTest),
    /// `Boolean.prototype.toString`.
    BooleanToString,
    /// `Boolean.prototype.valueOf`.
    BooleanValueOf,
    /// A `Date.prototype` method (`toString`, `getTime`, `valueOf`, ..).
    DateMethod(DateOp),
    /// `Date.prototype.toJSON`.
    DateToJson,
    /// A `%Map.prototype%` (`set == false`) / `%Set.prototype%` method with
    /// an inline body.
    CollectionOp {
        /// `%Set.prototype%` rather than `%Map.prototype%`.
        set: bool,
        /// The method.
        op: MapOp,
    },
    /// `Map.prototype.forEach` / `Set.prototype.forEach`.
    CollectionForEach {
        /// `%Set.prototype%` rather than `%Map.prototype%`.
        set: bool,
    },
    /// The `size` getter of `%Map.prototype%` / `%Set.prototype%`.
    CollectionSize {
        /// `%Set.prototype%` rather than `%Map.prototype%`.
        set: bool,
    },
    /// `Promise.prototype.then`.
    PromiseThen,
    /// `Promise.prototype.catch`.
    PromiseCatch,
    /// `Promise.prototype.finally`.
    PromiseFinally,
    /// `%GeneratorPrototype%.next`.
    GeneratorNext,
    /// `%GeneratorPrototype%.return`.
    GeneratorReturn,
    /// `%GeneratorPrototype%.throw`.
    GeneratorThrow,
    /// `%ThrowTypeError%`: the poison getter/setter of the strict `caller` /
    /// `arguments` properties (one identity for every installation).
    ThrowTypeError,
    /// The out-of-line generic method call of the guarded builtin call sites
    /// (DESIGN.md §6.4): `this` is the receiver, `env` packs the interned method
    /// key (low half) and the not-a-function message (high half). Not
    /// installed on any intrinsic.
    MethodCall,
    /// `GetIterator` of an [`IrNode::IterSource`]: `this` is the iterated
    /// value, `env` is `1` for a live `Map` / `Set` walk. Not installed on
    /// any intrinsic.
    IterSource,
    /// The global `isNaN`.
    IsNaN,
    /// The global `isFinite`.
    IsFinite,
    /// The global `parseFloat` (also `Number.parseFloat`).
    ParseFloat,
    /// The global `parseInt` (also `Number.parseInt`).
    ParseInt,
}

/// A `Number` predicate static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NumberTest {
    /// `Number.isInteger`.
    Integer,
    /// `Number.isSafeInteger`.
    SafeInteger,
    /// `Number.isFinite`.
    Finite,
    /// `Number.isNaN`.
    NaN,
}

impl NumberTest {
    /// The static's name.
    fn name(self) -> &'static str {
        match self {
            Self::Integer => "isInteger",
            Self::SafeInteger => "isSafeInteger",
            Self::Finite => "isFinite",
            Self::NaN => "isNaN",
        }
    }
}

/// The name of a `Math` method.
fn math_name(op: MathOp) -> &'static str {
    match op {
        MathOp::Floor => "floor",
        MathOp::Ceil => "ceil",
        MathOp::Round => "round",
        MathOp::Trunc => "trunc",
        MathOp::Abs => "abs",
        MathOp::Sqrt => "sqrt",
        MathOp::Min => "min",
        MathOp::Max => "max",
        MathOp::Pow => "pow",
        MathOp::Sign => "sign",
        MathOp::Hypot => "hypot",
        MathOp::Clz32 => "clz32",
        MathOp::Fround => "fround",
        MathOp::Imul => "imul",
        MathOp::Cbrt => "cbrt",
        MathOp::Log => "log",
        MathOp::Log2 => "log2",
        MathOp::Log10 => "log10",
        MathOp::Log1p => "log1p",
        MathOp::Exp => "exp",
        MathOp::Expm1 => "expm1",
        MathOp::Sin => "sin",
        MathOp::Cos => "cos",
        MathOp::Tan => "tan",
        MathOp::Asin => "asin",
        MathOp::Acos => "acos",
        MathOp::Atan => "atan",
        MathOp::Atan2 => "atan2",
        MathOp::Sinh => "sinh",
        MathOp::Cosh => "cosh",
        MathOp::Tanh => "tanh",
        MathOp::Asinh => "asinh",
        MathOp::Acosh => "acosh",
        MathOp::Atanh => "atanh",
        MathOp::Random => "random",
    }
}

/// A `String.prototype` method backed by a host `__method` op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StringOp {
    IndexOf,
    Includes,
    Slice,
    ToUpperCase,
    ToLowerCase,
    CharAt,
    Trim,
    TrimStart,
    TrimEnd,
    Repeat,
    StartsWith,
    EndsWith,
    PadStart,
    PadEnd,
    Substring,
    Split,
}

impl StringOp {
    /// The method's name.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::IndexOf => "indexOf",
            Self::Includes => "includes",
            Self::Slice => "slice",
            Self::ToUpperCase => "toUpperCase",
            Self::ToLowerCase => "toLowerCase",
            Self::CharAt => "charAt",
            Self::Trim => "trim",
            Self::TrimStart => "trimStart",
            Self::TrimEnd => "trimEnd",
            Self::Repeat => "repeat",
            Self::StartsWith => "startsWith",
            Self::EndsWith => "endsWith",
            Self::PadStart => "padStart",
            Self::PadEnd => "padEnd",
            Self::Substring => "substring",
            Self::Split => "split",
        }
    }

    /// The method's `length`.
    fn length(self) -> u32 {
        match self {
            Self::ToUpperCase
            | Self::ToLowerCase
            | Self::Trim
            | Self::TrimStart
            | Self::TrimEnd => 0,
            Self::Slice | Self::Substring | Self::Split => 2,
            _ => 1,
        }
    }

    /// The host `__method` op flag.
    pub(super) fn flag(self) -> i32 {
        match self {
            Self::IndexOf => abi::METHOD_OP_INDEX_OF,
            Self::Includes => abi::METHOD_OP_INCLUDES,
            Self::Slice => abi::METHOD_OP_SLICE,
            Self::ToUpperCase => abi::METHOD_OP_TO_UPPER,
            Self::ToLowerCase => abi::METHOD_OP_TO_LOWER,
            Self::CharAt => abi::METHOD_OP_CHAR_AT,
            Self::Trim => abi::METHOD_OP_TRIM,
            Self::TrimStart => abi::METHOD_OP_TRIM_START,
            Self::TrimEnd => abi::METHOD_OP_TRIM_END,
            Self::Repeat => abi::METHOD_OP_REPEAT,
            Self::StartsWith => abi::METHOD_OP_STARTS_WITH,
            Self::EndsWith => abi::METHOD_OP_ENDS_WITH,
            Self::PadStart => abi::METHOD_OP_PAD_START,
            Self::PadEnd => abi::METHOD_OP_PAD_END,
            Self::Substring => abi::METHOD_OP_SUBSTRING,
            Self::Split => abi::METHOD_OP_SPLIT,
        }
    }
}

impl Builtin {
    /// Every builtin, in funcref-table (and `__fn_info`) order.
    pub(super) const ALL: [Builtin; 149] = [
        Self::FunctionPrototype,
        Self::HasOwnProperty,
        Self::ObjectToString,
        Self::ValueOf,
        Self::Call,
        Self::Apply,
        Self::ErrorToString,
        Self::Math(MathOp::Floor),
        Self::Math(MathOp::Ceil),
        Self::Math(MathOp::Round),
        Self::Math(MathOp::Trunc),
        Self::Math(MathOp::Abs),
        Self::Math(MathOp::Sqrt),
        Self::Math(MathOp::Min),
        Self::Math(MathOp::Max),
        Self::Math(MathOp::Pow),
        Self::Math(MathOp::Sign),
        Self::Math(MathOp::Hypot),
        Self::Math(MathOp::Clz32),
        Self::Math(MathOp::Fround),
        Self::Math(MathOp::Imul),
        Self::Math(MathOp::Cbrt),
        Self::Math(MathOp::Log),
        Self::Math(MathOp::Log2),
        Self::Math(MathOp::Log10),
        Self::Math(MathOp::Log1p),
        Self::Math(MathOp::Exp),
        Self::Math(MathOp::Expm1),
        Self::Math(MathOp::Sin),
        Self::Math(MathOp::Cos),
        Self::Math(MathOp::Tan),
        Self::Math(MathOp::Asin),
        Self::Math(MathOp::Acos),
        Self::Math(MathOp::Atan),
        Self::Math(MathOp::Atan2),
        Self::Math(MathOp::Sinh),
        Self::Math(MathOp::Cosh),
        Self::Math(MathOp::Tanh),
        Self::Math(MathOp::Asinh),
        Self::Math(MathOp::Acosh),
        Self::Math(MathOp::Atanh),
        Self::Math(MathOp::Random),
        Self::JsonStringify,
        Self::JsonParse,
        Self::ConsoleLog,
        Self::Ctor(BuiltinCtor::Object),
        Self::Ctor(BuiltinCtor::Function),
        Self::Ctor(BuiltinCtor::Array),
        Self::Ctor(BuiltinCtor::String),
        Self::Ctor(BuiltinCtor::Number),
        Self::Ctor(BuiltinCtor::Boolean),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::Error)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::TypeError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::RangeError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::ReferenceError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::SyntaxError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::EvalError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::UriError)),
        Self::Ctor(BuiltinCtor::Error(ErrorKind::AggregateError)),
        Self::Ctor(BuiltinCtor::Map),
        Self::Ctor(BuiltinCtor::Set),
        Self::Ctor(BuiltinCtor::Promise),
        Self::Ctor(BuiltinCtor::Date),
        Self::ObjectStatic(ObjectBuiltinOp::Keys),
        Self::ObjectStatic(ObjectBuiltinOp::Values),
        Self::ObjectStatic(ObjectBuiltinOp::Entries),
        Self::IsArray,
        Self::PromiseResolve,
        Self::PromiseReject,
        Self::DateNow,
        Self::ArrayPush,
        Self::ArrayPop,
        Self::ArrayHof(ArrayHofOp::Map),
        Self::ArrayHof(ArrayHofOp::Filter),
        Self::ArrayHof(ArrayHofOp::Reduce),
        Self::ArrayHof(ArrayHofOp::ForEach),
        Self::ArrayIndexOf,
        Self::ArrayIncludes,
        Self::ArrayJoin,
        Self::ArraySlice,
        Self::ArrayToString,
        Self::StringMethod(StringOp::IndexOf),
        Self::StringMethod(StringOp::Includes),
        Self::StringMethod(StringOp::Slice),
        Self::StringMethod(StringOp::ToUpperCase),
        Self::StringMethod(StringOp::ToLowerCase),
        Self::StringMethod(StringOp::CharAt),
        Self::StringMethod(StringOp::Trim),
        Self::StringMethod(StringOp::TrimStart),
        Self::StringMethod(StringOp::TrimEnd),
        Self::StringMethod(StringOp::Repeat),
        Self::StringMethod(StringOp::StartsWith),
        Self::StringMethod(StringOp::EndsWith),
        Self::StringMethod(StringOp::PadStart),
        Self::StringMethod(StringOp::PadEnd),
        Self::StringMethod(StringOp::Substring),
        Self::StringMethod(StringOp::Split),
        Self::StringToString,
        Self::StringValueOf,
        Self::NumberToString,
        Self::NumberValueOf,
        Self::NumberToFixed,
        Self::NumberToExponential,
        Self::NumberToPrecision,
        Self::NumberToLocaleString,
        Self::NumberIs(NumberTest::Integer),
        Self::NumberIs(NumberTest::SafeInteger),
        Self::NumberIs(NumberTest::Finite),
        Self::NumberIs(NumberTest::NaN),
        Self::BooleanToString,
        Self::BooleanValueOf,
        Self::DateMethod(DateOp::ToString),
        Self::DateMethod(DateOp::GetTime),
        Self::DateMethod(DateOp::ValueOf),
        Self::DateMethod(DateOp::GetFullYear),
        Self::DateMethod(DateOp::GetMonth),
        Self::DateMethod(DateOp::GetDate),
        Self::DateMethod(DateOp::GetDay),
        Self::DateMethod(DateOp::GetHours),
        Self::DateMethod(DateOp::GetMinutes),
        Self::DateMethod(DateOp::GetSeconds),
        Self::DateMethod(DateOp::ToIsoString),
        Self::DateToJson,
        Self::CollectionOp {
            set: false,
            op: MapOp::Get,
        },
        Self::CollectionOp {
            set: false,
            op: MapOp::Set,
        },
        Self::CollectionOp {
            set: false,
            op: MapOp::Has,
        },
        Self::CollectionOp {
            set: false,
            op: MapOp::Delete,
        },
        Self::CollectionOp {
            set: false,
            op: MapOp::Clear,
        },
        Self::CollectionForEach { set: false },
        Self::CollectionSize { set: false },
        Self::CollectionOp {
            set: true,
            op: MapOp::Add,
        },
        Self::CollectionOp {
            set: true,
            op: MapOp::Has,
        },
        Self::CollectionOp {
            set: true,
            op: MapOp::Delete,
        },
        Self::CollectionOp {
            set: true,
            op: MapOp::Clear,
        },
        Self::CollectionForEach { set: true },
        Self::CollectionSize { set: true },
        Self::PromiseThen,
        Self::PromiseCatch,
        Self::PromiseFinally,
        Self::GeneratorNext,
        Self::GeneratorReturn,
        Self::GeneratorThrow,
        Self::ThrowTypeError,
        Self::MethodCall,
        Self::IterSource,
        Self::IsNaN,
        Self::IsFinite,
        Self::ParseFloat,
        Self::ParseInt,
    ];

    /// The function's `name`.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::FunctionPrototype
            | Self::ThrowTypeError
            | Self::MethodCall
            | Self::IterSource => "",
            Self::HasOwnProperty => "hasOwnProperty",
            Self::ObjectToString
            | Self::ErrorToString
            | Self::ArrayToString
            | Self::StringToString
            | Self::NumberToString
            | Self::BooleanToString => "toString",
            Self::ValueOf | Self::StringValueOf | Self::NumberValueOf | Self::BooleanValueOf => {
                "valueOf"
            }
            Self::DateMethod(op) => op.name(),
            Self::DateToJson => "toJSON",
            Self::CollectionOp { op, .. } => op.name(),
            Self::CollectionSize { .. } => "get size",
            Self::PromiseThen => "then",
            Self::PromiseCatch => "catch",
            Self::PromiseFinally => "finally",
            Self::GeneratorNext => "next",
            Self::GeneratorReturn => "return",
            Self::GeneratorThrow => "throw",
            Self::StringMethod(op) => op.name(),
            Self::Call => "call",
            Self::Apply => "apply",
            Self::Math(op) => math_name(op),
            Self::NumberToFixed => "toFixed",
            Self::NumberToExponential => "toExponential",
            Self::NumberToPrecision => "toPrecision",
            Self::NumberToLocaleString => "toLocaleString",
            Self::NumberIs(test) => test.name(),
            Self::ParseFloat => "parseFloat",
            Self::ParseInt => "parseInt",
            Self::JsonStringify => "stringify",
            Self::JsonParse => "parse",
            Self::ConsoleLog => "log",
            Self::Ctor(ctor) => ctor.name(),
            Self::ObjectStatic(ObjectBuiltinOp::Keys | ObjectBuiltinOp::ForInKeys) => "keys",
            Self::ObjectStatic(ObjectBuiltinOp::Values) => "values",
            Self::ObjectStatic(ObjectBuiltinOp::Entries) => "entries",
            Self::IsArray => "isArray",
            Self::PromiseResolve => "resolve",
            Self::PromiseReject => "reject",
            Self::DateNow => "now",
            Self::ArrayPush => "push",
            Self::ArrayPop => "pop",
            Self::ArrayHof(ArrayHofOp::Map) => "map",
            Self::ArrayHof(ArrayHofOp::Filter) => "filter",
            Self::ArrayHof(ArrayHofOp::Reduce) => "reduce",
            Self::ArrayHof(ArrayHofOp::ForEach) | Self::CollectionForEach { .. } => "forEach",
            Self::ArrayIndexOf => "indexOf",
            Self::ArrayIncludes => "includes",
            Self::ArrayJoin => "join",
            Self::ArraySlice => "slice",
            Self::IsNaN => "isNaN",
            Self::IsFinite => "isFinite",
        }
    }

    /// The function's `length`.
    pub(super) fn length(self) -> u32 {
        match self {
            Self::FunctionPrototype
            | Self::ThrowTypeError
            | Self::MethodCall
            | Self::IterSource
            | Self::StringToString
            | Self::StringValueOf
            | Self::NumberValueOf
            | Self::NumberToLocaleString
            | Self::Math(MathOp::Random)
            | Self::BooleanToString
            | Self::BooleanValueOf
            | Self::ObjectToString
            | Self::ValueOf
            | Self::ErrorToString
            | Self::ArrayToString
            | Self::DateMethod(_)
            | Self::CollectionSize { .. }
            | Self::CollectionOp {
                op: MapOp::Clear, ..
            }
            | Self::ConsoleLog
            | Self::DateNow
            | Self::ArrayPop
            | Self::Ctor(BuiltinCtor::Map | BuiltinCtor::Set) => 0,
            Self::Ctor(BuiltinCtor::Date) => 7,
            Self::Math(
                MathOp::Min | MathOp::Max | MathOp::Pow | MathOp::Hypot | MathOp::Atan2 | MathOp::Imul,
            )
            | Self::Ctor(BuiltinCtor::Error(ErrorKind::AggregateError))
            | Self::ParseInt
            | Self::Apply
            | Self::JsonParse
            | Self::ArraySlice
            | Self::CollectionOp { op: MapOp::Set, .. }
            | Self::PromiseThen => 2,
            Self::HasOwnProperty
            | Self::Call
            | Self::Math(_)
            | Self::Ctor(_)
            | Self::ObjectStatic(_)
            | Self::IsArray
            | Self::PromiseResolve
            | Self::PromiseReject
            | Self::ArrayPush
            | Self::ArrayHof(_)
            | Self::ArrayIndexOf
            | Self::ArrayIncludes
            | Self::ArrayJoin
            | Self::NumberToString
            | Self::NumberToFixed
            | Self::NumberToExponential
            | Self::NumberToPrecision
            | Self::NumberIs(_)
            | Self::ParseFloat
            | Self::IsNaN
            | Self::IsFinite
            | Self::DateToJson
            | Self::CollectionOp { .. }
            | Self::CollectionForEach { .. }
            | Self::PromiseCatch
            | Self::PromiseFinally
            | Self::GeneratorNext
            | Self::GeneratorReturn
            | Self::GeneratorThrow => 1,
            Self::StringMethod(op) => op.length(),
            Self::JsonStringify => 3,
        }
    }

    /// The prototype holding this builtin as a method (installed under its
    /// [`Self::name`]), if any.
    fn prototype(self) -> Option<Intrinsic> {
        let ctor = match self {
            Self::ArrayPush
            | Self::ArrayPop
            | Self::ArrayHof(_)
            | Self::ArrayIndexOf
            | Self::ArrayIncludes
            | Self::ArrayJoin
            | Self::ArraySlice
            | Self::ArrayToString => Some(BuiltinCtor::Array),
            Self::StringMethod(_) | Self::StringToString | Self::StringValueOf => {
                Some(BuiltinCtor::String)
            }
            Self::NumberToString
            | Self::NumberValueOf
            | Self::NumberToFixed
            | Self::NumberToExponential
            | Self::NumberToPrecision
            | Self::NumberToLocaleString => Some(BuiltinCtor::Number),
            Self::BooleanToString | Self::BooleanValueOf => Some(BuiltinCtor::Boolean),
            Self::DateMethod(_) | Self::DateToJson => Some(BuiltinCtor::Date),
            Self::CollectionOp { set, .. } | Self::CollectionForEach { set } => {
                Some(collection(set))
            }
            Self::PromiseThen | Self::PromiseCatch | Self::PromiseFinally => {
                Some(BuiltinCtor::Promise)
            }
            Self::GeneratorNext | Self::GeneratorReturn | Self::GeneratorThrow => {
                return Some(Intrinsic::GeneratorPrototype);
            }
            _ => None,
        };
        ctor.map(Intrinsic::Prototype)
    }

    /// The builtin's position in [`Self::ALL`]: its offset from the first
    /// builtin funcref-table slot and wasm function index.
    pub(super) fn index(self) -> u32 {
        (0u32..)
            .zip(Self::ALL)
            .find_map(|(index, b)| (b == self).then_some(index))
            .unwrap_or(0)
    }
}

/// A registry entry's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Value {
    /// A builtin method: its static function block.
    Method(Builtin),
    /// A static string.
    Str(&'static str),
    /// A number.
    Num(f64),
    /// `undefined`.
    Undefined,
    /// Another intrinsic object.
    Intrinsic(Intrinsic),
    /// An accessor property: its static `TYPE_ACCESSOR` block holding the
    /// method blocks of `get` and `set` (the entry has [`abi::ATTR_ACCESSOR`]).
    Accessor {
        /// The getter.
        get: Builtin,
        /// The setter (`undefined` when absent).
        set: Option<Builtin>,
    },
}

/// One own property of an intrinsic.
#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    /// The intrinsic owning the property.
    pub(super) intrinsic: Intrinsic,
    /// The property key (interned).
    pub(super) key: &'static str,
    /// The property attributes ([`abi::ATTR_WRITABLE`] ..).
    pub(super) attrs: u32,
    /// The property value.
    pub(super) value: Value,
}

/// Builtin methods and error `name`/`message`: writable, configurable, not
/// enumerable.
const METHOD_ATTRS: u32 = abi::ATTR_WRITABLE | abi::ATTR_CONFIGURABLE;

/// A function's `length`/`name`: configurable only.
const FN_FACT_ATTRS: u32 = abi::ATTR_CONFIGURABLE;

/// `AddRestrictedFunctionProperties`: the `caller` / `arguments` accessors of
/// `%Function.prototype%`, configurable only.
const POISON_ATTRS: u32 = abi::ATTR_ACCESSOR | abi::ATTR_CONFIGURABLE;

/// A builtin accessor property (`Map.prototype.size`): configurable, not
/// enumerable.
const ACCESSOR_ATTRS: u32 = abi::ATTR_ACCESSOR | abi::ATTR_CONFIGURABLE;

/// The `%ThrowTypeError%` getter and setter pair.
const POISON: Value = Value::Accessor {
    get: Builtin::ThrowTypeError,
    set: Some(Builtin::ThrowTypeError),
};

/// The `Map`/`Set` constructor of a collection.
fn collection(set: bool) -> BuiltinCtor {
    if set {
        BuiltinCtor::Set
    } else {
        BuiltinCtor::Map
    }
}

/// A constant (`Math.PI`, the global `NaN`/`Infinity`/`undefined`): no
/// attributes.
const CONST_ATTRS: u32 = 0;

/// The `Math` value properties.
const MATH_CONSTANTS: [(&str, f64); 8] = [
    ("E", std::f64::consts::E),
    ("LN10", std::f64::consts::LN_10),
    ("LN2", std::f64::consts::LN_2),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
];

/// The `Number` value properties.
const NUMBER_CONSTANTS: [(&str, f64); 8] = [
    ("EPSILON", f64::EPSILON),
    ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
    ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
    ("MAX_VALUE", f64::MAX),
    ("MIN_VALUE", 5e-324),
    ("NaN", f64::NAN),
    ("POSITIVE_INFINITY", f64::INFINITY),
    ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
];

/// The global object's own properties, one line per global binding (design
/// 4.2).
const GLOBALS: [(&str, u32, Value); 29] = [
    (
        "globalThis",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Global),
    ),
    ("Infinity", CONST_ATTRS, Value::Num(f64::INFINITY)),
    ("NaN", CONST_ATTRS, Value::Num(f64::NAN)),
    ("undefined", CONST_ATTRS, Value::Undefined),
    ("Math", METHOD_ATTRS, Value::Intrinsic(Intrinsic::Math)),
    ("JSON", METHOD_ATTRS, Value::Intrinsic(Intrinsic::Json)),
    (
        "console",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Console),
    ),
    (
        "Object",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Object)),
    ),
    (
        "Function",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Function)),
    ),
    (
        "Array",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Array)),
    ),
    (
        "String",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::String)),
    ),
    (
        "Number",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Number)),
    ),
    (
        "Boolean",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Boolean)),
    ),
    (
        "Error",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::Error))),
    ),
    (
        "TypeError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::TypeError))),
    ),
    (
        "RangeError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::RangeError))),
    ),
    (
        "ReferenceError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(
            ErrorKind::ReferenceError,
        ))),
    ),
    (
        "SyntaxError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::SyntaxError))),
    ),
    (
        "EvalError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::EvalError))),
    ),
    (
        "URIError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(ErrorKind::UriError))),
    ),
    (
        "AggregateError",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Error(
            ErrorKind::AggregateError,
        ))),
    ),
    (
        "Map",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Map)),
    ),
    (
        "Set",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Set)),
    ),
    (
        "Promise",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Promise)),
    ),
    (
        "Date",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Date)),
    ),
    ("isNaN", METHOD_ATTRS, Value::Method(Builtin::IsNaN)),
    ("isFinite", METHOD_ATTRS, Value::Method(Builtin::IsFinite)),
    ("parseFloat", METHOD_ATTRS, Value::Method(Builtin::ParseFloat)),
    ("parseInt", METHOD_ATTRS, Value::Method(Builtin::ParseInt)),
];

/// The registry: every own property of every intrinsic, in slot order per
/// intrinsic.
pub(super) fn registry() -> Vec<Entry> {
    use Intrinsic::{FunctionPrototype, ObjectPrototype};
    let entry = |intrinsic, key, attrs, value| Entry {
        intrinsic,
        key,
        attrs,
        value,
    };
    let mut entries = vec![
        entry(
            ObjectPrototype,
            "hasOwnProperty",
            METHOD_ATTRS,
            Value::Method(Builtin::HasOwnProperty),
        ),
        entry(
            ObjectPrototype,
            "toString",
            METHOD_ATTRS,
            Value::Method(Builtin::ObjectToString),
        ),
        entry(
            ObjectPrototype,
            "valueOf",
            METHOD_ATTRS,
            Value::Method(Builtin::ValueOf),
        ),
        entry(FunctionPrototype, "length", FN_FACT_ATTRS, Value::Num(0.0)),
        entry(FunctionPrototype, "name", FN_FACT_ATTRS, Value::Str("")),
        entry(
            FunctionPrototype,
            "call",
            METHOD_ATTRS,
            Value::Method(Builtin::Call),
        ),
        entry(
            FunctionPrototype,
            "apply",
            METHOD_ATTRS,
            Value::Method(Builtin::Apply),
        ),
        entry(FunctionPrototype, "caller", POISON_ATTRS, POISON),
        entry(FunctionPrototype, "arguments", POISON_ATTRS, POISON),
    ];
    for kind in ErrorKind::ALL {
        let proto = Intrinsic::ErrorPrototype(kind);
        entries.push(entry(proto, "name", METHOD_ATTRS, Value::Str(kind.name())));
        entries.push(entry(proto, "message", METHOD_ATTRS, Value::Str("")));
        if kind == ErrorKind::Error {
            entries.push(entry(
                proto,
                "toString",
                METHOD_ATTRS,
                Value::Method(Builtin::ErrorToString),
            ));
        }
    }
    for (key, value) in MATH_CONSTANTS {
        entries.push(entry(Intrinsic::Math, key, CONST_ATTRS, Value::Num(value)));
    }
    for builtin in Builtin::ALL {
        let namespace = match builtin {
            Builtin::Math(_) => Intrinsic::Math,
            Builtin::JsonStringify | Builtin::JsonParse => Intrinsic::Json,
            Builtin::ConsoleLog => Intrinsic::Console,
            _ => continue,
        };
        let method = Value::Method(builtin);
        entries.push(entry(namespace, builtin.name(), METHOD_ATTRS, method));
    }
    constructor_entries(&mut entries);
    entries.extend(GLOBALS.map(|(key, attrs, value)| entry(Intrinsic::Global, key, attrs, value)));
    entries
}

/// The constructors' own properties (`length`, `name`, the non-writable
/// `prototype`, statics), each prototype's `constructor` back-link, and the
/// prototype methods ([`Builtin::prototype`]).
fn constructor_entries(entries: &mut Vec<Entry>) {
    let entry = |intrinsic, key, attrs, value| Entry {
        intrinsic,
        key,
        attrs,
        value,
    };
    for ctor in BuiltinCtor::ALL {
        let (intrinsic, prototype) = (Intrinsic::Ctor(ctor), prototype_of(ctor));
        let length = f64::from(Builtin::Ctor(ctor).length());
        entries.push(entry(
            intrinsic,
            "length",
            FN_FACT_ATTRS,
            Value::Num(length),
        ));
        entries.push(entry(
            intrinsic,
            "name",
            FN_FACT_ATTRS,
            Value::Str(ctor.name()),
        ));
        entries.push(entry(
            intrinsic,
            "prototype",
            CONST_ATTRS,
            Value::Intrinsic(prototype),
        ));
        let statics: &[Builtin] = match ctor {
            BuiltinCtor::Object => &[
                Builtin::ObjectStatic(ObjectBuiltinOp::Keys),
                Builtin::ObjectStatic(ObjectBuiltinOp::Values),
                Builtin::ObjectStatic(ObjectBuiltinOp::Entries),
            ],
            BuiltinCtor::Array => &[Builtin::IsArray],
            BuiltinCtor::Promise => &[Builtin::PromiseResolve, Builtin::PromiseReject],
            BuiltinCtor::Date => &[Builtin::DateNow],
            BuiltinCtor::Number => &[
                Builtin::NumberIs(NumberTest::Integer),
                Builtin::NumberIs(NumberTest::SafeInteger),
                Builtin::NumberIs(NumberTest::Finite),
                Builtin::NumberIs(NumberTest::NaN),
                Builtin::ParseFloat,
                Builtin::ParseInt,
            ],
            _ => &[],
        };
        for &builtin in statics {
            let method = Value::Method(builtin);
            entries.push(entry(intrinsic, builtin.name(), METHOD_ATTRS, method));
        }
        if ctor == BuiltinCtor::Number {
            for (key, value) in NUMBER_CONSTANTS {
                entries.push(entry(intrinsic, key, CONST_ATTRS, Value::Num(value)));
            }
        }
        let ctor = Value::Intrinsic(intrinsic);
        entries.push(entry(prototype, "constructor", METHOD_ATTRS, ctor));
    }
    for builtin in Builtin::ALL {
        if let Some(prototype) = builtin.prototype() {
            let method = Value::Method(builtin);
            entries.push(entry(prototype, builtin.name(), METHOD_ATTRS, method));
        }
    }
    for set in [false, true] {
        let size = Value::Accessor {
            get: Builtin::CollectionSize { set },
            set: None,
        };
        let prototype = Intrinsic::Prototype(collection(set));
        entries.push(entry(prototype, "size", ACCESSOR_ATTRS, size));
    }
}

/// The builtin method `intrinsic.key` of the registry, if any.
pub(super) fn method_of(intrinsic: Intrinsic, key: &str) -> Option<Builtin> {
    registry().into_iter().find_map(|e| match e.value {
        Value::Method(builtin) if e.intrinsic == intrinsic && e.key == key => Some(builtin),
        _ => None,
    })
}

/// Every static string the image references (keys, string values, builtin
/// names). Interned in every program.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    registry()
        .into_iter()
        .flat_map(|e| {
            let value = match e.value {
                Value::Str(s) => Some(s),
                Value::Method(_)
                | Value::Num(_)
                | Value::Undefined
                | Value::Intrinsic(_)
                | Value::Accessor { .. } => None,
            };
            [Some(e.key), value]
        })
        .flatten()
        .chain(Builtin::ALL.map(Builtin::name))
}

/// The number of registry entries of `intrinsic`.
fn entry_count(registry: &[Entry], intrinsic: Intrinsic) -> u32 {
    let n = registry.iter().filter(|e| e.intrinsic == intrinsic).count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The byte size of `intrinsic`'s block plus its property table.
fn block_size(registry: &[Entry], intrinsic: Intrinsic) -> u32 {
    let prefix = if intrinsic.call().is_some() {
        abi::FUNC_BLOCK_SIZE
    } else {
        abi::OBJ_PREFIX_SIZE
    };
    prefix + abi::PROPS_HEADER_SIZE + abi::PROP_ENTRY_SIZE * entry_count(registry, intrinsic)
}

/// The builtins with a static method block (a method or an accessor half), in
/// image order.
fn methods(registry: &[Entry]) -> impl Iterator<Item = Builtin> + '_ {
    Builtin::ALL.into_iter().filter(|b| {
        registry.iter().any(|e| match e.value {
            Value::Method(m) => m == *b,
            Value::Accessor { get, set } => get == *b || set == Some(*b),
            _ => false,
        })
    })
}

/// The registry indices of the accessor entries, in image order.
fn accessors(registry: &[Entry]) -> impl Iterator<Item = usize> + '_ {
    (0..registry.len()).filter(|&i| matches!(registry[i].value, Value::Accessor { .. }))
}

/// The byte offset of `intrinsic`'s block from the image base.
fn intrinsic_offset(registry: &[Entry], intrinsic: Intrinsic) -> u32 {
    intrinsics()
        .take_while(|i| *i != intrinsic)
        .map(|i| block_size(registry, i))
        .sum()
}

/// The byte offset of the method block of `builtin` from the image base.
fn method_offset(registry: &[Entry], builtin: Builtin) -> u32 {
    let objects: u32 = intrinsics().map(|i| block_size(registry, i)).sum();
    let before = methods(registry).take_while(|b| *b != builtin).count();
    objects + abi::FUNC_BLOCK_SIZE * u32::try_from(before).unwrap_or(0)
}

/// The byte offset of the accessor block of registry entry `entry` from the
/// image base.
fn accessor_offset(registry: &[Entry], entry: usize) -> u32 {
    let methods = u32::try_from(methods(registry).count()).unwrap_or(0);
    let objects: u32 = intrinsics().map(|i| block_size(registry, i)).sum();
    let before = accessors(registry).take_while(|i| *i != entry).count();
    objects
        + abi::FUNC_BLOCK_SIZE * methods
        + abi::ACCESSOR_BLOCK_SIZE * u32::try_from(before).unwrap_or(0)
}

/// The byte size of the whole image (a multiple of 8, like every block).
pub(super) fn image_size() -> i32 {
    let registry = registry();
    let methods = u32::try_from(methods(&registry).count()).unwrap_or(0);
    let accessors = u32::try_from(accessors(&registry).count()).unwrap_or(0);
    let size = intrinsics().map(|i| block_size(&registry, i)).sum::<u32>()
        + abi::FUNC_BLOCK_SIZE * methods
        + abi::ACCESSOR_BLOCK_SIZE * accessors;
    size.cast_signed()
}

/// The heap-pointer jsval of `intrinsic` in the image at `base`.
pub(super) fn intrinsic_addr_bits(base: i32, intrinsic: Intrinsic) -> i64 {
    let addr = base.cast_unsigned() + intrinsic_offset(&registry(), intrinsic);
    HEAP_BITS | i64::from(addr)
}

/// The heap-pointer jsval of `intrinsic` (a compile-time constant).
pub(super) fn intrinsic_bits(intrinsic: Intrinsic, helpers: Helpers<'_>) -> i64 {
    intrinsic_addr_bits(helpers.realm(), intrinsic)
}

/// The heap-pointer jsval of `builtin`'s static method block (a
/// compile-time constant).
pub(super) fn method_bits(builtin: Builtin, helpers: Helpers<'_>) -> i64 {
    let addr = helpers.realm().cast_unsigned() + method_offset(&registry(), builtin);
    HEAP_BITS | i64::from(addr)
}

/// Build the image bytes for `base`: `first_slot` is the funcref-table slot
/// of [`Builtin::ALL`]'s first entry, `strings` the static-string offsets.
pub(super) fn image(base: i32, first_slot: u32, strings: &HashMap<String, i32>) -> Vec<u8> {
    let registry = registry();
    let at = |s: &str| strings.get(s).copied().unwrap_or(HEAP_BASE).cast_unsigned();
    let heap = |addr: u32| abi::HEAP | u64::from(addr);
    let base = base.cast_unsigned();
    let function_proto = heap(base + intrinsic_offset(&registry, Intrinsic::FunctionPrototype));
    let mut blob = Vec::new();
    // A `TYPE_FUNCTION` block's payload after its prefix: the funcref slot, a
    // reserved word, and an `undefined` environment.
    let fn_payload = |blob: &mut Vec<u8>, builtin: Builtin| {
        blob.extend_from_slice(&(first_slot + builtin.index()).to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&abi::UNDEFINED.to_le_bytes());
    };
    for intrinsic in intrinsics() {
        let addr = base + intrinsic_offset(&registry, intrinsic);
        let proto = intrinsic
            .proto()
            .map_or(abi::NULL, |p| heap(base + intrinsic_offset(&registry, p)));
        let (header, prefix) = match intrinsic.call() {
            Some(_) => (
                [abi::TYPE_FUNCTION, 0, intrinsic.flags(), 0],
                abi::FUNC_BLOCK_SIZE,
            ),
            None => (
                [abi::TYPE_OBJECT, 0, 0, intrinsic.class()],
                abi::OBJ_PREFIX_SIZE,
            ),
        };
        blob.extend_from_slice(&header);
        blob.extend_from_slice(&(addr + prefix).to_le_bytes());
        blob.extend_from_slice(&proto.to_le_bytes());
        if let Some(builtin) = intrinsic.call() {
            fn_payload(&mut blob, builtin);
        }
        let count = entry_count(&registry, intrinsic);
        let (watched, bit) = intrinsic
            .watch_bit()
            .map_or((0, 0), |bit| (abi::PROPS_WATCHED, bit));
        blob.extend_from_slice(&[abi::TYPE_PROPS, 0, abi::PROPS_HAS_NON_ENUM | watched, bit]);
        for field in [count, count, count] {
            blob.extend_from_slice(&field.to_le_bytes());
        }
        for (i, e) in registry
            .iter()
            .enumerate()
            .filter(|(_, e)| e.intrinsic == intrinsic)
        {
            let value = match e.value {
                Value::Method(b) => heap(base + method_offset(&registry, b)),
                Value::Str(s) => heap(at(s)),
                Value::Num(n) if n.is_nan() => abi::QNAN,
                Value::Num(n) => n.to_bits(),
                Value::Undefined => abi::UNDEFINED,
                Value::Intrinsic(i) => heap(base + intrinsic_offset(&registry, i)),
                Value::Accessor { .. } => heap(base + accessor_offset(&registry, i)),
            };
            blob.extend_from_slice(&at(e.key).to_le_bytes());
            blob.extend_from_slice(&e.attrs.to_le_bytes());
            blob.extend_from_slice(&value.to_le_bytes());
        }
    }
    for builtin in methods(&registry) {
        blob.extend_from_slice(&[abi::TYPE_FUNCTION, 0, abi::FUNC_FLAG_LAZY, 0]);
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&function_proto.to_le_bytes());
        fn_payload(&mut blob, builtin);
    }
    for i in accessors(&registry) {
        if let Value::Accessor { get, set } = registry[i].value {
            blob.extend_from_slice(&[abi::TYPE_ACCESSOR, 0, 0, 0]);
            blob.extend_from_slice(&0u32.to_le_bytes());
            for half in [Some(get), set] {
                let method = half.map_or(abi::UNDEFINED, |half| {
                    heap(base + method_offset(&registry, half))
                });
                blob.extend_from_slice(&method.to_le_bytes());
            }
        }
    }
    blob
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Static-string offsets for every realm string, as `collect_strings`
    /// would assign them.
    fn offsets() -> HashMap<String, i32> {
        (0..)
            .zip(strings())
            .map(|(i, s)| (s.to_owned(), HEAP_BASE + 16 * i))
            .collect()
    }

    fn u32_at(image: &[u8], at: u32) -> u32 {
        let at = at as usize;
        u32::from_le_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]])
    }

    fn u64_at(image: &[u8], at: u32) -> u64 {
        u64::from(u32_at(image, at)) | u64::from(u32_at(image, at + 4)) << 32
    }

    #[test]
    fn image_is_deterministic_and_addresses_are_constant() {
        let base = 4096;
        let strings = offsets();
        let image = image(base, 9, &strings);
        assert_eq!(
            image,
            super::image(base, 9, &strings),
            "deterministic bytes"
        );
        assert_eq!(image.len(), usize::try_from(image_size()).unwrap_or(0));
        assert_eq!(image_size() % 8, 0, "8-aligned size");
        let registry = registry();
        for intrinsic in intrinsics() {
            let offset = intrinsic_offset(&registry, intrinsic);
            assert_eq!(offset % 8, 0, "{intrinsic:?}: 8-aligned");
            // The address is the base plus a layout-only offset.
            let bits = intrinsic_addr_bits(base, intrinsic);
            assert_eq!(intrinsic_addr_bits(base + 64, intrinsic), bits + 64);
            assert_eq!(bits, HEAP_BITS | i64::from(base + offset.cast_signed()));
            // Header, `props` right behind the prefix, `proto` per the registry.
            let callable = intrinsic.call().is_some();
            let kind = if callable {
                abi::TYPE_FUNCTION
            } else {
                abi::TYPE_OBJECT
            };
            assert_eq!(image[offset as usize], kind, "{intrinsic:?}: kind");
            let prefix = if callable {
                abi::FUNC_BLOCK_SIZE
            } else {
                abi::OBJ_PREFIX_SIZE
            };
            let props = u32_at(&image, offset + abi::OBJ_PROPS_OFFSET);
            assert_eq!(props, base.cast_unsigned() + offset + prefix);
            let proto = u64_at(&image, offset + abi::OBJ_PROTO_OFFSET);
            let expected = intrinsic
                .proto()
                .map_or(abi::NULL, |p| intrinsic_addr_bits(base, p).cast_unsigned());
            assert_eq!(proto, expected, "{intrinsic:?}: proto");
            // A full table of the intrinsic's entries, keyed by interned strings.
            let table = offset + prefix;
            let count = entry_count(&registry, intrinsic);
            assert_eq!(image[table as usize], abi::TYPE_PROPS);
            let tflags = image[(table + abi::PROPS_TFLAGS_OFFSET) as usize];
            let bit = image[(table + abi::PROPS_WATCH_BIT_OFFSET) as usize];
            match intrinsic.watch_bit() {
                Some(expected) => {
                    assert_ne!(tflags & abi::PROPS_WATCHED, 0, "{intrinsic:?}: watched");
                    assert_eq!(bit, expected, "{intrinsic:?}: dirty bit");
                }
                None => assert_eq!((tflags & abi::PROPS_WATCHED, bit), (0, 0)),
            }
            assert_eq!(u32_at(&image, table + abi::PROPS_CAP_OFFSET), count);
            for (k, e) in (0..).zip(registry.iter().filter(|e| e.intrinsic == intrinsic)) {
                let entry = table + abi::PROPS_HEADER_SIZE + abi::PROP_ENTRY_SIZE * k;
                let key = u32_at(&image, entry + abi::PROP_KEY_OFFSET);
                assert_eq!(Some(key.cast_signed()), strings.get(e.key).copied());
            }
        }
        // Every method block is a lazy function inheriting %Function.prototype%,
        // pointing at its builtin's funcref slot.
        let function_proto = intrinsic_addr_bits(base, Intrinsic::FunctionPrototype);
        for builtin in methods(&registry) {
            let at = method_offset(&registry, builtin);
            assert_eq!(
                &image[at as usize..at as usize + 4],
                &[abi::TYPE_FUNCTION, 0, abi::FUNC_FLAG_LAZY, 0]
            );
            assert_eq!(
                u64_at(&image, at + abi::OBJ_PROTO_OFFSET),
                function_proto.cast_unsigned()
            );
            assert_eq!(
                u32_at(&image, at + abi::FUNC_INDEX_OFFSET),
                9 + builtin.index()
            );
        }
    }

    #[test]
    fn restricted_function_properties_share_one_throw_type_error() {
        let (base, strings) = (4096, offsets());
        let image = image(base, 9, &strings);
        let registry = registry();
        let thrower = (HEAP_BITS
            | i64::from(base + method_offset(&registry, Builtin::ThrowTypeError).cast_signed()))
        .cast_unsigned();
        let table =
            intrinsic_offset(&registry, Intrinsic::FunctionPrototype) + abi::FUNC_BLOCK_SIZE;
        let mut found = Vec::new();
        for (k, e) in (0..).zip(
            registry
                .iter()
                .filter(|e| e.intrinsic == Intrinsic::FunctionPrototype),
        ) {
            if e.key != "caller" && e.key != "arguments" {
                continue;
            }
            let entry = table + abi::PROPS_HEADER_SIZE + abi::PROP_ENTRY_SIZE * k;
            assert_eq!(
                u32_at(&image, entry + abi::PROP_ATTRS_OFFSET),
                abi::ATTR_ACCESSOR | abi::ATTR_CONFIGURABLE,
                "{}: attrs",
                e.key
            );
            let block = u32_at(&image, entry + abi::PROP_VALUE_OFFSET) - base.cast_unsigned();
            assert_eq!(image[block as usize], abi::TYPE_ACCESSOR);
            // get === set === the one %ThrowTypeError% for both properties.
            assert_eq!(u64_at(&image, block + abi::ACCESSOR_GET_OFFSET), thrower);
            assert_eq!(u64_at(&image, block + abi::ACCESSOR_SET_OFFSET), thrower);
            found.push(e.key);
        }
        assert_eq!(found, ["caller", "arguments"]);
        // %ThrowTypeError% is a plain (non-constructor) function: length 0, name "".
        assert_eq!(Builtin::ThrowTypeError.length(), 0);
        assert_eq!(Builtin::ThrowTypeError.name(), "");
        let at = method_offset(&registry, Builtin::ThrowTypeError) as usize;
        assert_eq!(image[at + 2] & abi::FUNC_FLAG_CONSTRUCTOR, 0);
    }

    #[test]
    fn emitted_module_is_deterministic() {
        let program = crate::ir::IrProgram {
            locals: 0,
            stmts: Vec::new(),
            functions: Vec::new(),
            module_cells: 0,
            arity: 2,
        };
        assert_eq!(super::super::emit(&program), super::super::emit(&program));
    }
}
