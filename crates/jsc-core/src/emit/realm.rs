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
//! [`Value::Accessor`] entry, holding its getter/setter method blocks, and
//! last one static `TYPE_SYMBOL` block per [`WellKnown`] symbol. A static
//! table that a user mutation overflows is rebuilt on the heap by `__define`
//! like any other.

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
    /// `Date`, `Symbol`): `constructor` and the prototype methods.
    Prototype(BuiltinCtor),
    /// `%IteratorPrototype%`: `[@@iterator]() { return this }`; the `proto`
    /// of every iterator prototype and of `%GeneratorPrototype%`.
    IteratorPrototype,
    /// `%ArrayIteratorPrototype%` / `%StringIteratorPrototype%` /
    /// `%MapIteratorPrototype%` / `%SetIteratorPrototype%`: the `next` of the
    /// family's `CLASS_ITERATOR` objects.
    IterPrototype(IterFamily),
    /// `%AsyncIteratorPrototype%`: `[@@asyncIterator]() { return this }`;
    /// `%AsyncGeneratorPrototype%`'s `proto`.
    AsyncIteratorPrototype,
    /// `%AsyncFromSyncIteratorPrototype%` (design/symbol-strings.md §5.8):
    /// the promise-returning `next`, `return`, `throw` of the
    /// `CreateAsyncFromSyncIterator` objects; not reachable from the global
    /// object.
    AsyncFromSyncIteratorPrototype,
    /// `%GeneratorPrototype%`: `next`, `return`, `throw`; every generator
    /// object's `proto`.
    GeneratorPrototype,
    /// `%AsyncGeneratorPrototype%`: the promise-returning `next`, `return`,
    /// `throw` of every async generator object.
    AsyncGeneratorPrototype,
    /// The hidden `Symbol.for` registry (`proto` `null`, unreachable from
    /// the global object): its table maps interned key strings to the
    /// registered symbols.
    SymbolRegistry,
    /// `Array.prototype[@@unscopables]`: a `null`-prototype object whose
    /// own data properties (each `true`) name the methods `with` skips.
    ArrayUnscopables,
    /// `%WrapForValidIteratorPrototype%`: the `next` / `return` of the
    /// wrappers `Iterator.from` creates (design/symbol-strings.md §5.7).
    WrapForValidIteratorPrototype,
    /// `%IteratorHelperPrototype%`: the `next` / `return` of the helper
    /// objects the lazy `Iterator.prototype` methods create (§5.7).
    IteratorHelperPrototype,
}

/// A builtin iterator family: the receivers of one iterator prototype's
/// `next` (design/symbol-strings.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IterFamily {
    Array,
    String,
    Map,
    Set,
}

impl IterFamily {
    /// Every family, in image order.
    pub(super) const ALL: [IterFamily; 4] = [Self::Array, Self::String, Self::Map, Self::Set];
}

/// What an Array, Map or Set iterator yields per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IterKind {
    Keys,
    Values,
    Entries,
}

impl IterKind {
    /// The creating method's name.
    fn name(self) -> &'static str {
        match self {
            Self::Keys => "keys",
            Self::Values => "values",
            Self::Entries => "entries",
        }
    }
}

/// The constructors whose prototype is an [`Intrinsic::Prototype`].
const PLAIN_PROTOTYPES: [BuiltinCtor; 9] = [
    BuiltinCtor::Array,
    BuiltinCtor::String,
    BuiltinCtor::Number,
    BuiltinCtor::Boolean,
    BuiltinCtor::Map,
    BuiltinCtor::Set,
    BuiltinCtor::Promise,
    BuiltinCtor::Date,
    BuiltinCtor::Symbol,
];

/// A well-known symbol (`design/symbol-strings.md` §4): a static
/// `TYPE_SYMBOL` block of the image whose description is
/// `"Symbol.<name>"`, the value of the non-writable, non-configurable
/// `Symbol.<name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WellKnown {
    AsyncIterator,
    HasInstance,
    IsConcatSpreadable,
    Iterator,
    Match,
    MatchAll,
    Replace,
    Search,
    Species,
    Split,
    ToPrimitive,
    ToStringTag,
    Unscopables,
}

impl WellKnown {
    /// Every well-known symbol, in image order.
    const ALL: [WellKnown; 13] = [
        Self::AsyncIterator,
        Self::HasInstance,
        Self::IsConcatSpreadable,
        Self::Iterator,
        Self::Match,
        Self::MatchAll,
        Self::Replace,
        Self::Search,
        Self::Species,
        Self::Split,
        Self::ToPrimitive,
        Self::ToStringTag,
        Self::Unscopables,
    ];

    /// The `Symbol` static's key.
    fn name(self) -> &'static str {
        match self {
            Self::AsyncIterator => "asyncIterator",
            Self::HasInstance => "hasInstance",
            Self::IsConcatSpreadable => "isConcatSpreadable",
            Self::Iterator => "iterator",
            Self::Match => "match",
            Self::MatchAll => "matchAll",
            Self::Replace => "replace",
            Self::Search => "search",
            Self::Species => "species",
            Self::Split => "split",
            Self::ToPrimitive => "toPrimitive",
            Self::ToStringTag => "toStringTag",
            Self::Unscopables => "unscopables",
        }
    }

    /// The symbol's `description`.
    fn description(self) -> &'static str {
        match self {
            Self::AsyncIterator => "Symbol.asyncIterator",
            Self::HasInstance => "Symbol.hasInstance",
            Self::IsConcatSpreadable => "Symbol.isConcatSpreadable",
            Self::Iterator => "Symbol.iterator",
            Self::Match => "Symbol.match",
            Self::MatchAll => "Symbol.matchAll",
            Self::Replace => "Symbol.replace",
            Self::Search => "Symbol.search",
            Self::Species => "Symbol.species",
            Self::Split => "Symbol.split",
            Self::ToPrimitive => "Symbol.toPrimitive",
            Self::ToStringTag => "Symbol.toStringTag",
            Self::Unscopables => "Symbol.unscopables",
        }
    }

    /// The symbol's position in [`Self::ALL`].
    fn index(self) -> u32 {
        (0u32..)
            .zip(Self::ALL)
            .find_map(|(index, w)| (w == self).then_some(index))
            .unwrap_or(0)
    }
}

/// The `prototype` intrinsic of `ctor`.
pub(super) fn prototype_of(ctor: BuiltinCtor) -> Intrinsic {
    match ctor {
        BuiltinCtor::Object => Intrinsic::ObjectPrototype,
        BuiltinCtor::Function => Intrinsic::FunctionPrototype,
        BuiltinCtor::Error(kind) => Intrinsic::ErrorPrototype(kind),
        BuiltinCtor::Iterator => Intrinsic::IteratorPrototype,
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
    pub(super) fn proto(self) -> Option<Intrinsic> {
        match self {
            Self::ObjectPrototype | Self::SymbolRegistry | Self::ArrayUnscopables => None,
            Self::FunctionPrototype
            | Self::ErrorPrototype(ErrorKind::Error)
            | Self::Global
            | Self::Math
            | Self::Json
            | Self::Console
            | Self::Prototype(_)
            | Self::IteratorPrototype
            | Self::AsyncIteratorPrototype => Some(Self::ObjectPrototype),
            Self::IterPrototype(_)
            | Self::GeneratorPrototype
            | Self::WrapForValidIteratorPrototype
            | Self::IteratorHelperPrototype => Some(Self::IteratorPrototype),
            Self::AsyncGeneratorPrototype | Self::AsyncFromSyncIteratorPrototype => {
                Some(Self::AsyncIteratorPrototype)
            }
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
            Self::Prototype(BuiltinCtor::Symbol) => 14,
            Self::IteratorPrototype => 15,
            Self::IterPrototype(IterFamily::Array) => 16,
            Self::IterPrototype(IterFamily::String) => 17,
            Self::IterPrototype(IterFamily::Map) => 18,
            Self::IterPrototype(IterFamily::Set) => 19,
            // `%Array%[@@species]` / `%Promise%[@@species]`
            // (design/symbol-strings.md §4).
            Self::Ctor(BuiltinCtor::Array) => 20,
            Self::Ctor(BuiltinCtor::Promise) => 21,
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
        .chain([Intrinsic::IteratorPrototype])
        .chain(IterFamily::ALL.map(Intrinsic::IterPrototype))
        .chain([
            Intrinsic::AsyncIteratorPrototype,
            Intrinsic::AsyncFromSyncIteratorPrototype,
            Intrinsic::GeneratorPrototype,
            Intrinsic::AsyncGeneratorPrototype,
            Intrinsic::SymbolRegistry,
            Intrinsic::ArrayUnscopables,
            Intrinsic::WrapForValidIteratorPrototype,
            Intrinsic::IteratorHelperPrototype,
        ])
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
    /// `Object.prototype.isPrototypeOf`.
    IsPrototypeOf,
    /// `Object.prototype.propertyIsEnumerable`.
    PropertyIsEnumerable,
    /// `Object.prototype.toLocaleString`.
    ToLocaleString,
    /// `Object.prototype.__defineGetter__` / `__defineSetter__`.
    DefineAccessor {
        /// `__defineSetter__` rather than `__defineGetter__`.
        set: bool,
    },
    /// `Object.prototype.__lookupGetter__` / `__lookupSetter__`.
    LookupAccessor {
        /// `__lookupSetter__` rather than `__lookupGetter__`.
        set: bool,
    },
    /// The getter of `Object.prototype.__proto__`.
    ProtoGet,
    /// The setter of `Object.prototype.__proto__`.
    ProtoSet,
    /// `Function.prototype.call`.
    Call,
    /// `Function.prototype.apply`.
    Apply,
    /// `Function.prototype.bind`.
    Bind,
    /// `Function.prototype.toString`.
    FunctionToString,
    /// `Function.prototype[@@hasInstance]` (`OrdinaryHasInstance`).
    FunctionHasInstance,
    /// The call/construct trampoline of every bound function (DESIGN.md
    /// §5.5): `env` is the `[target, boundThis, ...boundArgs]` array. Not
    /// installed on any intrinsic.
    BoundCall,
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
    /// An `Object` static over property descriptors and integrity levels.
    ObjectReflect(ReflectOp),
    /// `Array.isArray`.
    IsArray,
    /// `Array.of`.
    ArrayOf,
    /// `Array.from`.
    ArrayFrom,
    /// A `Promise` static.
    PromiseStatic(PromiseStaticOp),
    /// `Math.sumPrecise`.
    MathSumPrecise,
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
    /// `Array.prototype.toLocaleString`.
    ArrayToLocaleString,
    /// `Array.prototype.at`.
    ArrayAt,
    /// `Array.prototype.find` / `findIndex` / `findLast` / `findLastIndex` /
    /// `some` / `every`.
    ArrayFind(ArrayFindOp),
    /// `Array.prototype.reduceRight`.
    ArrayReduceRight,
    /// `Array.prototype.lastIndexOf`.
    ArrayLastIndexOf,
    /// `Array.prototype.concat`.
    ArrayConcat,
    /// `Array.prototype.flat` (`map == false`) / `flatMap`.
    ArrayFlat {
        /// `flatMap`: map each top-level element first.
        map: bool,
    },
    /// `Array.prototype.splice`.
    ArraySplice,
    /// `Array.prototype.sort`.
    ArraySort,
    /// `Array.prototype.reverse`.
    ArrayReverse,
    /// `Array.prototype.shift`.
    ArrayShift,
    /// `Array.prototype.unshift`.
    ArrayUnshift,
    /// `Array.prototype.fill`.
    ArrayFill,
    /// `Array.prototype.copyWithin`.
    ArrayCopyWithin,
    /// `Array.prototype.with` / `toReversed` / `toSorted` / `toSpliced`.
    ArrayCopy(ArrayCopyOp),
    /// A generic `String.prototype` method ([`StringOp`]).
    StringMethod(StringOp),
    /// `String.prototype.toString`.
    StringToString,
    /// `String.prototype.valueOf`.
    StringValueOf,
    /// `String.fromCharCode`.
    StringFromCharCode,
    /// `String.fromCodePoint`.
    StringFromCodePoint,
    /// `String.raw`.
    StringRaw,
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
    /// A generic `%Date.prototype%` method (any receiver).
    DateGeneric(DateGenericOp),
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
    /// `%AsyncGeneratorPrototype%.next` / `return` / `throw`: queue the
    /// request and return its promise.
    AsyncGeneratorMethod(AsyncGenRequest),
    /// Serve an async generator's queued requests (argument 0 is the
    /// generator) until it suspends on an `await` or the queue empties. Not
    /// installed on any intrinsic.
    AsyncGeneratorDrain,
    /// Resume an async generator's body once (argument 0 is the generator,
    /// argument 1 the sent value, thrown at the suspend when `env` is
    /// `true`) and settle the front request from the outcome. Not installed
    /// on any intrinsic.
    AsyncGeneratorStep,
    /// A promise reaction of an async generator (`env` is the generator):
    /// the settlement of an `await`, or of a `return()` value. Not installed
    /// on any intrinsic.
    AsyncGeneratorReaction(AsyncGenReaction),
    /// `%ThrowTypeError%`: the poison getter/setter of the strict `caller` /
    /// `arguments` properties (one identity for every installation).
    ThrowTypeError,
    /// The out-of-line generic method call of the guarded builtin call sites
    /// (DESIGN.md §6.4): `this` is the receiver, `env` packs the interned method
    /// key (low half) and the not-a-function message (high half). Not
    /// installed on any intrinsic.
    MethodCall,
    /// An internal builtin of the iteration protocol
    /// (design/symbol-strings.md §5.1). Not installed on any intrinsic.
    Protocol(ProtocolOp),
    /// The global `isNaN`.
    IsNaN,
    /// The global `isFinite`.
    IsFinite,
    /// The global `parseFloat` (also `Number.parseFloat`).
    ParseFloat,
    /// The global `parseInt` (also `Number.parseInt`).
    ParseInt,
    /// A `Symbol` static or `%Symbol.prototype%` builtin.
    Symbol(SymbolOp),
    /// An iteration builtin (design/symbol-strings.md §5.4, §5.8).
    Iter(IterOp),
    /// A `@@species` builtin: a `get [Symbol.species]`, or an internal
    /// closure of the promise `SpeciesConstructor` paths.
    Species(SpeciesOp),
    /// An `Iterator` static, `%WrapForValidIteratorPrototype%` method or
    /// `%IteratorPrototype%` accessor half (design/symbol-strings.md §5.7).
    Iterator(IteratorOp),
}

/// The `Iterator` builtins of design/symbol-strings.md §5.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IteratorOp {
    /// `Iterator.from`.
    From,
    /// `%WrapForValidIteratorPrototype%.next`.
    WrapNext,
    /// `%WrapForValidIteratorPrototype%.return`.
    WrapReturn,
    /// The getter of `Iterator.prototype.constructor`: `%Iterator%`.
    ConstructorGet,
    /// The setter of `Iterator.prototype.constructor`
    /// (`SetterThatIgnoresPrototypeProperties`).
    ConstructorSet,
    /// The getter of `Iterator.prototype[@@toStringTag]`: `"Iterator"`.
    TagGet,
    /// The setter of `Iterator.prototype[@@toStringTag]`
    /// (`SetterThatIgnoresPrototypeProperties`).
    TagSet,
    /// A lazy `Iterator.prototype` helper: a fresh helper object.
    Helper(HelperKind),
    /// `%IteratorHelperPrototype%.next`.
    HelperNext,
    /// `%IteratorHelperPrototype%.return`.
    HelperReturn,
    /// An eager `Iterator.prototype` helper: consumes the iterator.
    Eager(EagerKind),
}

/// The eager `Iterator.prototype` helpers (design/symbol-strings.md §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EagerKind {
    /// `reduce(reducer[, initialValue])`.
    Reduce,
    /// `toArray()`.
    ToArray,
    /// `forEach(fn)`.
    ForEach,
    /// `some(predicate)`.
    Some,
    /// `every(predicate)`.
    Every,
    /// `find(predicate)`.
    Find,
}

impl EagerKind {
    /// The method's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::Reduce => "reduce",
            Self::ToArray => "toArray",
            Self::ForEach => "forEach",
            Self::Some => "some",
            Self::Every => "every",
            Self::Find => "find",
        }
    }
}

/// The lazy `Iterator.prototype` helpers (design/symbol-strings.md §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HelperKind {
    /// `map(mapper)`.
    Map,
    /// `filter(predicate)`.
    Filter,
    /// `take(limit)`.
    Take,
    /// `drop(limit)`.
    Drop,
    /// `flatMap(mapper)`.
    FlatMap,
}

impl HelperKind {
    /// The method's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Filter => "filter",
            Self::Take => "take",
            Self::Drop => "drop",
            Self::FlatMap => "flatMap",
        }
    }
}

impl IteratorOp {
    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [IteratorOp; 20] = [
        Self::From,
        Self::WrapNext,
        Self::WrapReturn,
        Self::ConstructorGet,
        Self::ConstructorSet,
        Self::TagGet,
        Self::TagSet,
        Self::Helper(HelperKind::Map),
        Self::Helper(HelperKind::Filter),
        Self::Helper(HelperKind::Take),
        Self::Helper(HelperKind::Drop),
        Self::Helper(HelperKind::FlatMap),
        Self::HelperNext,
        Self::HelperReturn,
        Self::Eager(EagerKind::Reduce),
        Self::Eager(EagerKind::ToArray),
        Self::Eager(EagerKind::ForEach),
        Self::Eager(EagerKind::Some),
        Self::Eager(EagerKind::Every),
        Self::Eager(EagerKind::Find),
    ];

    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::WrapNext | Self::HelperNext => "next",
            Self::WrapReturn | Self::HelperReturn => "return",
            Self::ConstructorGet => "get constructor",
            Self::ConstructorSet => "set constructor",
            Self::TagGet => "get [Symbol.toStringTag]",
            Self::TagSet => "set [Symbol.toStringTag]",
            Self::Helper(kind) => kind.name(),
            Self::Eager(kind) => kind.name(),
        }
    }

    /// The builtin's `length`.
    fn length(self) -> u32 {
        match self {
            Self::From | Self::ConstructorSet | Self::TagSet | Self::Helper(_) => 1,
            Self::Eager(kind) => u32::from(kind != EagerKind::ToArray),
            Self::WrapNext
            | Self::WrapReturn
            | Self::ConstructorGet
            | Self::TagGet
            | Self::HelperNext
            | Self::HelperReturn => 0,
        }
    }

    /// The prototype holding this builtin under its [`Self::name`].
    fn prototype(self) -> Option<Intrinsic> {
        match self {
            Self::WrapNext | Self::WrapReturn => Some(Intrinsic::WrapForValidIteratorPrototype),
            Self::Helper(_) | Self::Eager(_) => Some(Intrinsic::IteratorPrototype),
            Self::HelperNext | Self::HelperReturn => Some(Intrinsic::IteratorHelperPrototype),
            _ => None,
        }
    }
}

/// The `Promise` statics, in installation order.
const PROMISE_STATICS: [Builtin; 6] = [
    Builtin::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::All)),
    Builtin::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::AllSettled)),
    Builtin::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::Any)),
    Builtin::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::Race)),
    Builtin::PromiseStatic(PromiseStaticOp::Resolve),
    Builtin::PromiseStatic(PromiseStaticOp::Reject),
];

/// The `Promise` statics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromiseStaticOp {
    /// `Promise.resolve`.
    Resolve,
    /// `Promise.reject`.
    Reject,
    /// `Promise.all` / `allSettled` / `any` / `race`.
    Combinator(CombinatorOp),
}

impl PromiseStaticOp {
    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Reject => "reject",
            Self::Combinator(op) => op.name(),
        }
    }
}

/// The `Promise` combinators over an iterable (design/symbol-strings.md
/// §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CombinatorOp {
    /// `Promise.all`.
    All,
    /// `Promise.allSettled`.
    AllSettled,
    /// `Promise.any`.
    Any,
    /// `Promise.race`.
    Race,
}

impl CombinatorOp {
    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [CombinatorOp; 4] = [Self::All, Self::AllSettled, Self::Any, Self::Race];

    /// The builtin's `name`.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::AllSettled => "allSettled",
            Self::Any => "any",
            Self::Race => "race",
        }
    }
}

/// The `@@species` builtins: the `get [Symbol.species]` accessors, and the
/// internal closures of the `SpeciesConstructor` paths of
/// `Promise.prototype.then` / `finally` and of the `Promise` combinators
/// (each a fresh function block whose `env` holds its captured values; not
/// installed on any intrinsic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpeciesOp {
    /// The `get [Symbol.species]` of `%Array%` / `%Map%` / `%Set%` /
    /// `%Promise%`: returns `this`.
    Getter(BuiltinCtor),
    /// `GetCapabilitiesExecutor(resolve, reject)`: `env` is the capability
    /// record array `[resolve, reject]`.
    Executor,
    /// The reaction of a `then` whose result promise is a species instance:
    /// `env` is `[handler, resolve, reject]` (`reject` = the rejection
    /// reaction).
    Reaction {
        /// The rejection reaction rather than the fulfillment one.
        reject: bool,
    },
    /// `thenFinally` / `catchFinally` (`reject`) of `finally`: `env` is
    /// `[onFinally, C]`.
    Finally {
        /// `catchFinally` rather than `thenFinally`.
        reject: bool,
    },
    /// The `() => value` / `() => throw reason` (`throw`) closure of
    /// `thenFinally` / `catchFinally`: `env` is the value.
    Thunk {
        /// Throw the value rather than return it.
        throw: bool,
    },
    /// A resolve / reject element function of a `Promise` combinator: `env`
    /// is `[state, index, alreadyCalled]`, `state` the combinator's shared
    /// `[values, remainingElements, settle]` (`settle` the capability's
    /// `resolve`, or its `reject` for `any`).
    Element {
        /// The combinator (`all`, `allSettled` or `any`).
        combinator: CombinatorOp,
        /// The rejection element (`allSettled`'s `onRejected`, `any`'s
        /// reject element) rather than the fulfillment one.
        reject: bool,
    },
}

impl SpeciesOp {
    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [SpeciesOp; 15] = [
        Self::Getter(BuiltinCtor::Array),
        Self::Getter(BuiltinCtor::Map),
        Self::Getter(BuiltinCtor::Set),
        Self::Getter(BuiltinCtor::Promise),
        Self::Executor,
        Self::Reaction { reject: false },
        Self::Reaction { reject: true },
        Self::Finally { reject: false },
        Self::Finally { reject: true },
        Self::Thunk { throw: false },
        Self::Thunk { throw: true },
        Self::Element {
            combinator: CombinatorOp::All,
            reject: false,
        },
        Self::Element {
            combinator: CombinatorOp::AllSettled,
            reject: false,
        },
        Self::Element {
            combinator: CombinatorOp::AllSettled,
            reject: true,
        },
        Self::Element {
            combinator: CombinatorOp::Any,
            reject: true,
        },
    ];

    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::Getter(_) => "get [Symbol.species]",
            _ => "",
        }
    }

    /// The builtin's `length`.
    fn length(self) -> u32 {
        match self {
            Self::Executor => 2,
            Self::Reaction { .. } | Self::Finally { .. } | Self::Element { .. } => 1,
            Self::Getter(_) | Self::Thunk { .. } => 0,
        }
    }
}

/// `Array.fromAsync` (design/symbol-strings.md §5.8) and the internal
/// builtins of its promise-reaction state machine (not installed on any
/// intrinsic), whose `env` is the loop's state block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FromAsyncOp {
    /// `Array.fromAsync(items, mapFn, thisArg)`: returns a promise.
    Entry,
    /// One step of the loop, resumed at the phase the state block records;
    /// a throw is left pending for the caller to reject (or close) with.
    Run,
    /// The fulfillment reaction of each await: stores the value, then runs.
    Resume,
    /// The rejection reaction of each await: rejects the result promise,
    /// closing the iterator first while a mapped value is in flight.
    Reject,
    /// The reaction of the awaited `return()` result of a close: rejects the
    /// result promise with the stored reason.
    Rethrow,
}

impl FromAsyncOp {
    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [FromAsyncOp; 5] = [
        Self::Entry,
        Self::Run,
        Self::Resume,
        Self::Reject,
        Self::Rethrow,
    ];

    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::Entry => "fromAsync",
            _ => "",
        }
    }

    /// The builtin's `length`.
    fn length(self) -> u32 {
        match self {
            Self::Run => 0,
            _ => 1,
        }
    }
}

/// An async iteration builtin (design/symbol-strings.md §5.8): the
/// `%AsyncFromSyncIteratorPrototype%` methods, and the internal builtins
/// behind the async record nodes and their promise reactions (not installed
/// on any intrinsic), called with `this` and `env` as below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AsyncIterOp {
    /// `GetIterator(this, async)`: `this` is the iterated value; returns
    /// its async iterator (an async-from-sync wrapper over its sync
    /// iterator when it has no `@@asyncIterator`).
    Open,
    /// The call of an async `IteratorStep`: `this` is the iterator, `env`
    /// its cached `next`; returns the (unawaited) call result.
    Step,
    /// The check of an async `IteratorStep`: `this` is the awaited step
    /// result; a `TypeError` unless an object, else the hole when
    /// `ToBoolean(this.done)`, else `this`.
    Complete,
    /// The call of `AsyncIteratorClose`: `this` is the iterator;
    /// `GetMethod(this, "return")` (the hole when absent) and its
    /// (unawaited) call result.
    Return,
    /// `%AsyncFromSyncIteratorPrototype%.next` / `return` / `throw`.
    FromSync(AsyncGenRequest),
    /// The fulfillment reaction of `AsyncFromSyncIteratorContinuation`
    /// (`env` is `done`): `v => ({ value: v, done })`.
    Unwrap,
    /// The rejection reaction of `AsyncFromSyncIteratorContinuation`
    /// (`env` is the sync iterator): close it, then rethrow the reason.
    CloseRejected,
    /// `Array.fromAsync` and its internal builtins.
    FromAsync(FromAsyncOp),
}

impl AsyncIterOp {
    /// The builtin's `name` (the internal ones are anonymous).
    fn name(self) -> &'static str {
        match self {
            Self::FromSync(request) => request.name(),
            Self::FromAsync(op) => op.name(),
            _ => "",
        }
    }

    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [AsyncIterOp; 14] = [
        Self::Open,
        Self::Step,
        Self::Complete,
        Self::Return,
        Self::FromSync(AsyncGenRequest::Next),
        Self::FromSync(AsyncGenRequest::Return),
        Self::FromSync(AsyncGenRequest::Throw),
        Self::Unwrap,
        Self::CloseRejected,
        Self::FromAsync(FromAsyncOp::ALL[0]),
        Self::FromAsync(FromAsyncOp::ALL[1]),
        Self::FromAsync(FromAsyncOp::ALL[2]),
        Self::FromAsync(FromAsyncOp::ALL[3]),
        Self::FromAsync(FromAsyncOp::ALL[4]),
    ];
}

/// An internal builtin of the iteration protocol (design/symbol-strings.md
/// §5.1), called by the record nodes and the consumers with `this` and
/// `env` as below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProtocolOp {
    /// The protocol half of `GetIterator`: `this` is the iterated value,
    /// `env` its `@@iterator` method (`<x> is not iterable` unless
    /// callable); returns the iterator the call yields (a `TypeError` unless
    /// an object).
    Open,
    /// `IteratorStep`: `this` is the iterator, `env` its cached `next`;
    /// returns the result object, or the hole once `done`.
    Step,
    /// `IteratorClose` of a non-throw completion: `this` is the iterator;
    /// `GetMethod(this, "return")`, its call and the object check of its
    /// result.
    Close,
    /// `IterableToList`: `this` is the iterated value; returns a fresh array
    /// of the values its iterator yields.
    ToList,
}

/// An iteration builtin: an iterator-creating method, a builtin iterator
/// prototype's `next`, or a `[@@iterator]` returning `this`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IterOp {
    /// `%IteratorPrototype%[@@iterator]`: returns `this`.
    IteratorSelf,
    /// `%AsyncIteratorPrototype%[@@asyncIterator]`: returns `this`.
    AsyncIteratorSelf,
    /// `Array.prototype.keys` / `values` (also `[@@iterator]`) / `entries`.
    ArrayIter(IterKind),
    /// `Map.prototype.keys` / `values` / `entries` (also `[@@iterator]`), or
    /// `Set.prototype.values` (also `keys` and `[@@iterator]`) / `entries`.
    CollectionIter {
        /// `%Set.prototype%` rather than `%Map.prototype%`.
        set: bool,
        /// What the iterator yields.
        kind: IterKind,
    },
    /// `String.prototype[@@iterator]`.
    StringIterator,
    /// The `next` of a builtin iterator prototype.
    Next(IterFamily),
    /// An async iteration builtin (design/symbol-strings.md §5.8).
    Async(AsyncIterOp),
}

impl IterOp {
    /// Every op but [`Self::Async`], in [`Builtin::ALL`] order.
    const ALL: [IterOp; 15] = [
        Self::IteratorSelf,
        Self::AsyncIteratorSelf,
        Self::ArrayIter(IterKind::Keys),
        Self::ArrayIter(IterKind::Values),
        Self::ArrayIter(IterKind::Entries),
        Self::CollectionIter {
            set: false,
            kind: IterKind::Keys,
        },
        Self::CollectionIter {
            set: false,
            kind: IterKind::Values,
        },
        Self::CollectionIter {
            set: false,
            kind: IterKind::Entries,
        },
        Self::CollectionIter {
            set: true,
            kind: IterKind::Values,
        },
        Self::CollectionIter {
            set: true,
            kind: IterKind::Entries,
        },
        Self::StringIterator,
        Self::Next(IterFamily::Array),
        Self::Next(IterFamily::String),
        Self::Next(IterFamily::Map),
        Self::Next(IterFamily::Set),
    ];

    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::IteratorSelf | Self::StringIterator => "[Symbol.iterator]",
            Self::AsyncIteratorSelf => "[Symbol.asyncIterator]",
            Self::ArrayIter(kind) | Self::CollectionIter { kind, .. } => kind.name(),
            Self::Next(_) => "next",
            Self::Async(op) => op.name(),
        }
    }

    /// The builtin's `length`.
    fn length(self) -> u32 {
        match self {
            Self::Async(
                AsyncIterOp::FromSync(_) | AsyncIterOp::Unwrap | AsyncIterOp::CloseRejected,
            ) => 1,
            Self::Async(AsyncIterOp::FromAsync(op)) => op.length(),
            _ => 0,
        }
    }

    /// The prototype holding this builtin under its string [`Self::name`]
    /// (the `[@@iterator]` ones are installed by `iterator_entries`).
    fn prototype(self) -> Option<Intrinsic> {
        match self {
            Self::ArrayIter(_) => Some(Intrinsic::Prototype(BuiltinCtor::Array)),
            Self::CollectionIter { set, .. } => Some(Intrinsic::Prototype(collection(set))),
            Self::Next(family) => Some(Intrinsic::IterPrototype(family)),
            Self::Async(AsyncIterOp::FromSync(_)) => {
                Some(Intrinsic::AsyncFromSyncIteratorPrototype)
            }
            Self::IteratorSelf
            | Self::AsyncIteratorSelf
            | Self::StringIterator
            | Self::Async(_) => None,
        }
    }
}

/// A generic `%Date.prototype%` method: its receiver need not be a Date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DateGenericOp {
    /// `Date.prototype.toJSON`.
    ToJson,
    /// `Date.prototype[@@toPrimitive]` (installed under its symbol key).
    ToPrimitive,
}

impl DateGenericOp {
    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::ToJson => "toJSON",
            Self::ToPrimitive => "[Symbol.toPrimitive]",
        }
    }
}

/// A `Symbol` static or `%Symbol.prototype%` builtin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolOp {
    /// `Symbol.for`.
    For,
    /// `Symbol.keyFor`.
    KeyFor,
    /// `Symbol.prototype.toString`.
    ToString,
    /// `Symbol.prototype.valueOf`.
    ValueOf,
    /// The getter of `Symbol.prototype.description`.
    Description,
    /// `Symbol.prototype[@@toPrimitive]`.
    ToPrimitive,
}

impl SymbolOp {
    /// The builtin's `name`.
    fn name(self) -> &'static str {
        match self {
            Self::For => "for",
            Self::KeyFor => "keyFor",
            Self::ToString => "toString",
            Self::ValueOf => "valueOf",
            Self::Description => "get description",
            Self::ToPrimitive => "[Symbol.toPrimitive]",
        }
    }

    /// The builtin's `length`.
    fn length(self) -> u32 {
        match self {
            Self::For | Self::KeyFor | Self::ToPrimitive => 1,
            Self::ToString | Self::ValueOf | Self::Description => 0,
        }
    }
}

/// An async generator request (`%AsyncGeneratorPrototype%` method).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AsyncGenRequest {
    /// `next(v)`.
    Next,
    /// `throw(e)`.
    Throw,
    /// `return(v)`.
    Return,
}

impl AsyncGenRequest {
    /// The method's name.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Throw => "throw",
            Self::Return => "return",
        }
    }
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

/// The name of a `%Function.prototype%` method.
fn function_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Call => "call",
        Builtin::Apply => "apply",
        Builtin::Bind => "bind",
        Builtin::FunctionHasInstance => "[Symbol.hasInstance]",
        _ => "",
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

/// An `Array.prototype` method that calls a predicate per index until it
/// decides the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrayFindOp {
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
    Some,
    Every,
}

impl ArrayFindOp {
    /// The method's name.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Find => "find",
            Self::FindIndex => "findIndex",
            Self::FindLast => "findLast",
            Self::FindLastIndex => "findLastIndex",
            Self::Some => "some",
            Self::Every => "every",
        }
    }
}

/// An `Object` static over property descriptors, integrity levels,
/// prototypes and own keys (DESIGN.md §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReflectOp {
    DefineProperty,
    DefineProperties,
    GetOwnPropertyDescriptor,
    GetOwnPropertyDescriptors,
    PreventExtensions,
    IsExtensible,
    Seal,
    Freeze,
    IsSealed,
    IsFrozen,
    Create,
    GetPrototypeOf,
    SetPrototypeOf,
    GetOwnPropertyNames,
    GetOwnPropertySymbols,
    Assign,
    HasOwn,
    Is,
    FromEntries,
}

impl ReflectOp {
    /// Every op, in [`Builtin::ALL`] order.
    const ALL: [ReflectOp; 19] = [
        Self::DefineProperty,
        Self::DefineProperties,
        Self::GetOwnPropertyDescriptor,
        Self::GetOwnPropertyDescriptors,
        Self::PreventExtensions,
        Self::IsExtensible,
        Self::Seal,
        Self::Freeze,
        Self::IsSealed,
        Self::IsFrozen,
        Self::Create,
        Self::GetPrototypeOf,
        Self::SetPrototypeOf,
        Self::GetOwnPropertyNames,
        Self::GetOwnPropertySymbols,
        Self::Assign,
        Self::HasOwn,
        Self::Is,
        Self::FromEntries,
    ];

    /// The static's name.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::DefineProperty => "defineProperty",
            Self::DefineProperties => "defineProperties",
            Self::GetOwnPropertyDescriptor => "getOwnPropertyDescriptor",
            Self::GetOwnPropertyDescriptors => "getOwnPropertyDescriptors",
            Self::PreventExtensions => "preventExtensions",
            Self::IsExtensible => "isExtensible",
            Self::Seal => "seal",
            Self::Freeze => "freeze",
            Self::IsSealed => "isSealed",
            Self::IsFrozen => "isFrozen",
            Self::Create => "create",
            Self::GetPrototypeOf => "getPrototypeOf",
            Self::SetPrototypeOf => "setPrototypeOf",
            Self::GetOwnPropertyNames => "getOwnPropertyNames",
            Self::GetOwnPropertySymbols => "getOwnPropertySymbols",
            Self::Assign => "assign",
            Self::HasOwn => "hasOwn",
            Self::Is => "is",
            Self::FromEntries => "fromEntries",
        }
    }

    /// The static's `length`.
    fn length(self) -> u32 {
        match self {
            Self::DefineProperty => 3,
            Self::DefineProperties
            | Self::GetOwnPropertyDescriptor
            | Self::Create
            | Self::SetPrototypeOf
            | Self::Assign
            | Self::HasOwn
            | Self::Is => 2,
            _ => 1,
        }
    }
}
/// An `Array.prototype` method that returns a changed copy of its receiver as
/// a plain array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrayCopyOp {
    With,
    ToReversed,
    ToSorted,
    ToSpliced,
}

impl ArrayCopyOp {
    /// The method's name.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::With => "with",
            Self::ToReversed => "toReversed",
            Self::ToSorted => "toSorted",
            Self::ToSpliced => "toSpliced",
        }
    }
}

/// The promise reactions an async generator registers (see
/// [`Builtin::AsyncGeneratorReaction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AsyncGenReaction {
    /// An awaited value fulfilled: resume the body with it.
    AwaitFulfilled,
    /// An awaited value rejected: throw the reason at the `await`.
    AwaitRejected,
    /// A `return(v)` value fulfilled: the request completes `{ v, done }`.
    ReturnFulfilled,
    /// A `return(v)` value rejected: the request rejects.
    ReturnRejected,
}

/// A generic `String.prototype` method: the case mappings, `localeCompare`
/// and `normalize` are backed by a host `__method` op ([`StringOp::flag`]);
/// the rest run in the guest on code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StringOp {
    IndexOf,
    Includes,
    Slice,
    ToUpperCase,
    ToLowerCase,
    ToLocaleUpperCase,
    ToLocaleLowerCase,
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
    At,
    CharCodeAt,
    CodePointAt,
    Concat,
    LastIndexOf,
    LocaleCompare,
    Normalize,
    Search,
    Match,
    MatchAll,
    Replace,
    ReplaceAll,
    IsWellFormed,
    ToWellFormed,
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
            Self::ToLocaleUpperCase => "toLocaleUpperCase",
            Self::ToLocaleLowerCase => "toLocaleLowerCase",
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
            Self::At => "at",
            Self::CharCodeAt => "charCodeAt",
            Self::CodePointAt => "codePointAt",
            Self::Concat => "concat",
            Self::LastIndexOf => "lastIndexOf",
            Self::LocaleCompare => "localeCompare",
            Self::Normalize => "normalize",
            Self::Search => "search",
            Self::Match => "match",
            Self::MatchAll => "matchAll",
            Self::Replace => "replace",
            Self::ReplaceAll => "replaceAll",
            Self::IsWellFormed => "isWellFormed",
            Self::ToWellFormed => "toWellFormed",
        }
    }

    /// The method's `length`.
    fn length(self) -> u32 {
        match self {
            Self::ToUpperCase
            | Self::ToLowerCase
            | Self::ToLocaleUpperCase
            | Self::ToLocaleLowerCase
            | Self::Trim
            | Self::TrimStart
            | Self::TrimEnd
            | Self::Normalize
            | Self::IsWellFormed
            | Self::ToWellFormed => 0,
            Self::Slice | Self::Substring | Self::Split | Self::Replace | Self::ReplaceAll => 2,
            _ => 1,
        }
    }

    /// The host `__method` op flag of a host-backed method.
    pub(super) fn flag(self) -> Option<i32> {
        Some(match self {
            Self::ToUpperCase | Self::ToLocaleUpperCase => abi::METHOD_OP_TO_UPPER,
            Self::ToLowerCase | Self::ToLocaleLowerCase => abi::METHOD_OP_TO_LOWER,
            Self::LocaleCompare => abi::METHOD_OP_LOCALE_COMPARE,
            Self::Normalize => abi::METHOD_OP_NORMALIZE,
            _ => return None,
        })
    }
}

impl Builtin {
    /// Every builtin, in funcref-table (and `__fn_info`) order.
    pub(super) const ALL: [Builtin; 316] = [
        Self::FunctionPrototype,
        Self::HasOwnProperty,
        Self::ObjectToString,
        Self::ValueOf,
        Self::IsPrototypeOf,
        Self::PropertyIsEnumerable,
        Self::ToLocaleString,
        Self::DefineAccessor { set: false },
        Self::DefineAccessor { set: true },
        Self::LookupAccessor { set: false },
        Self::LookupAccessor { set: true },
        Self::ProtoGet,
        Self::ProtoSet,
        Self::Call,
        Self::Apply,
        Self::Bind,
        Self::FunctionToString,
        Self::FunctionHasInstance,
        Self::BoundCall,
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
        Self::Ctor(BuiltinCtor::Symbol),
        Self::Ctor(BuiltinCtor::Iterator),
        Self::ObjectStatic(ObjectBuiltinOp::Keys),
        Self::ObjectStatic(ObjectBuiltinOp::Values),
        Self::ObjectStatic(ObjectBuiltinOp::Entries),
        Self::ObjectReflect(ReflectOp::ALL[0]),
        Self::ObjectReflect(ReflectOp::ALL[1]),
        Self::ObjectReflect(ReflectOp::ALL[2]),
        Self::ObjectReflect(ReflectOp::ALL[3]),
        Self::ObjectReflect(ReflectOp::ALL[4]),
        Self::ObjectReflect(ReflectOp::ALL[5]),
        Self::ObjectReflect(ReflectOp::ALL[6]),
        Self::ObjectReflect(ReflectOp::ALL[7]),
        Self::ObjectReflect(ReflectOp::ALL[8]),
        Self::ObjectReflect(ReflectOp::ALL[9]),
        Self::ObjectReflect(ReflectOp::ALL[10]),
        Self::ObjectReflect(ReflectOp::ALL[11]),
        Self::ObjectReflect(ReflectOp::ALL[12]),
        Self::ObjectReflect(ReflectOp::ALL[13]),
        Self::ObjectReflect(ReflectOp::ALL[14]),
        Self::ObjectReflect(ReflectOp::ALL[15]),
        Self::ObjectReflect(ReflectOp::ALL[16]),
        Self::ObjectReflect(ReflectOp::ALL[17]),
        Self::ObjectReflect(ReflectOp::ALL[18]),
        Self::IsArray,
        Self::ArrayOf,
        Self::ArrayFrom,
        Self::PromiseStatic(PromiseStaticOp::Resolve),
        Self::PromiseStatic(PromiseStaticOp::Reject),
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
        Self::ArrayToLocaleString,
        Self::ArrayAt,
        Self::ArrayFind(ArrayFindOp::Find),
        Self::ArrayFind(ArrayFindOp::FindIndex),
        Self::ArrayFind(ArrayFindOp::FindLast),
        Self::ArrayFind(ArrayFindOp::FindLastIndex),
        Self::ArrayFind(ArrayFindOp::Some),
        Self::ArrayFind(ArrayFindOp::Every),
        Self::ArrayReduceRight,
        Self::ArrayLastIndexOf,
        Self::ArrayConcat,
        Self::ArrayFlat { map: false },
        Self::ArrayFlat { map: true },
        Self::ArraySplice,
        Self::ArraySort,
        Self::ArrayReverse,
        Self::ArrayShift,
        Self::ArrayUnshift,
        Self::ArrayFill,
        Self::ArrayCopyWithin,
        Self::ArrayCopy(ArrayCopyOp::With),
        Self::ArrayCopy(ArrayCopyOp::ToReversed),
        Self::ArrayCopy(ArrayCopyOp::ToSorted),
        Self::ArrayCopy(ArrayCopyOp::ToSpliced),
        Self::StringMethod(StringOp::IndexOf),
        Self::StringMethod(StringOp::Includes),
        Self::StringMethod(StringOp::Slice),
        Self::StringMethod(StringOp::ToUpperCase),
        Self::StringMethod(StringOp::ToLowerCase),
        Self::StringMethod(StringOp::ToLocaleUpperCase),
        Self::StringMethod(StringOp::ToLocaleLowerCase),
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
        Self::StringMethod(StringOp::At),
        Self::StringMethod(StringOp::CharCodeAt),
        Self::StringMethod(StringOp::CodePointAt),
        Self::StringMethod(StringOp::Concat),
        Self::StringMethod(StringOp::LastIndexOf),
        Self::StringMethod(StringOp::LocaleCompare),
        Self::StringMethod(StringOp::Normalize),
        Self::StringMethod(StringOp::Search),
        Self::StringMethod(StringOp::Match),
        Self::StringMethod(StringOp::MatchAll),
        Self::StringMethod(StringOp::Replace),
        Self::StringMethod(StringOp::ReplaceAll),
        Self::StringMethod(StringOp::IsWellFormed),
        Self::StringMethod(StringOp::ToWellFormed),
        Self::StringToString,
        Self::StringValueOf,
        Self::StringFromCharCode,
        Self::StringFromCodePoint,
        Self::StringRaw,
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
        Self::DateGeneric(DateGenericOp::ToJson),
        Self::DateGeneric(DateGenericOp::ToPrimitive),
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
        Self::AsyncGeneratorMethod(AsyncGenRequest::Next),
        Self::AsyncGeneratorMethod(AsyncGenRequest::Return),
        Self::AsyncGeneratorMethod(AsyncGenRequest::Throw),
        Self::AsyncGeneratorDrain,
        Self::AsyncGeneratorStep,
        Self::AsyncGeneratorReaction(AsyncGenReaction::AwaitFulfilled),
        Self::AsyncGeneratorReaction(AsyncGenReaction::AwaitRejected),
        Self::AsyncGeneratorReaction(AsyncGenReaction::ReturnFulfilled),
        Self::AsyncGeneratorReaction(AsyncGenReaction::ReturnRejected),
        Self::ThrowTypeError,
        Self::MethodCall,
        Self::Protocol(ProtocolOp::Open),
        Self::Protocol(ProtocolOp::Step),
        Self::Protocol(ProtocolOp::Close),
        Self::Protocol(ProtocolOp::ToList),
        Self::IsNaN,
        Self::IsFinite,
        Self::ParseFloat,
        Self::ParseInt,
        Self::Symbol(SymbolOp::For),
        Self::Symbol(SymbolOp::KeyFor),
        Self::Symbol(SymbolOp::ToString),
        Self::Symbol(SymbolOp::ValueOf),
        Self::Symbol(SymbolOp::Description),
        Self::Symbol(SymbolOp::ToPrimitive),
        Self::Iter(IterOp::ALL[0]),
        Self::Iter(IterOp::ALL[1]),
        Self::Iter(IterOp::ALL[2]),
        Self::Iter(IterOp::ALL[3]),
        Self::Iter(IterOp::ALL[4]),
        Self::Iter(IterOp::ALL[5]),
        Self::Iter(IterOp::ALL[6]),
        Self::Iter(IterOp::ALL[7]),
        Self::Iter(IterOp::ALL[8]),
        Self::Iter(IterOp::ALL[9]),
        Self::Iter(IterOp::ALL[10]),
        Self::Iter(IterOp::ALL[11]),
        Self::Iter(IterOp::ALL[12]),
        Self::Iter(IterOp::ALL[13]),
        Self::Iter(IterOp::ALL[14]),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[0])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[1])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[2])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[3])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[4])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[5])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[6])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[7])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[8])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[9])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[10])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[11])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[12])),
        Self::Iter(IterOp::Async(AsyncIterOp::ALL[13])),
        Self::Species(SpeciesOp::ALL[0]),
        Self::Species(SpeciesOp::ALL[1]),
        Self::Species(SpeciesOp::ALL[2]),
        Self::Species(SpeciesOp::ALL[3]),
        Self::Species(SpeciesOp::ALL[4]),
        Self::Species(SpeciesOp::ALL[5]),
        Self::Species(SpeciesOp::ALL[6]),
        Self::Species(SpeciesOp::ALL[7]),
        Self::Species(SpeciesOp::ALL[8]),
        Self::Species(SpeciesOp::ALL[9]),
        Self::Species(SpeciesOp::ALL[10]),
        Self::Iterator(IteratorOp::ALL[0]),
        Self::Iterator(IteratorOp::ALL[1]),
        Self::Iterator(IteratorOp::ALL[2]),
        Self::Iterator(IteratorOp::ALL[3]),
        Self::Iterator(IteratorOp::ALL[4]),
        Self::Iterator(IteratorOp::ALL[5]),
        Self::Iterator(IteratorOp::ALL[6]),
        Self::Iterator(IteratorOp::ALL[7]),
        Self::Iterator(IteratorOp::ALL[8]),
        Self::Iterator(IteratorOp::ALL[9]),
        Self::Iterator(IteratorOp::ALL[10]),
        Self::Iterator(IteratorOp::ALL[11]),
        Self::Iterator(IteratorOp::ALL[12]),
        Self::Iterator(IteratorOp::ALL[13]),
        Self::Iterator(IteratorOp::ALL[14]),
        Self::Iterator(IteratorOp::ALL[15]),
        Self::Iterator(IteratorOp::ALL[16]),
        Self::Iterator(IteratorOp::ALL[17]),
        Self::Iterator(IteratorOp::ALL[18]),
        Self::Iterator(IteratorOp::ALL[19]),
        Self::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::ALL[0])),
        Self::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::ALL[1])),
        Self::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::ALL[2])),
        Self::PromiseStatic(PromiseStaticOp::Combinator(CombinatorOp::ALL[3])),
        Self::Species(SpeciesOp::ALL[11]),
        Self::Species(SpeciesOp::ALL[12]),
        Self::Species(SpeciesOp::ALL[13]),
        Self::Species(SpeciesOp::ALL[14]),
        Self::MathSumPrecise,
    ];

    /// The function's `name`.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::FunctionPrototype
            | Self::ThrowTypeError
            | Self::MethodCall
            | Self::BoundCall
            | Self::Protocol(_)
            | Self::AsyncGeneratorDrain
            | Self::AsyncGeneratorStep
            | Self::AsyncGeneratorReaction(_) => "",
            Self::ProtoGet => "get __proto__",
            Self::ProtoSet => "set __proto__",
            Self::CollectionSize { .. } => "get size",
            Self::Species(op) => op.name(),
            Self::Iterator(op) => op.name(),
            method => method.method_name(),
        }
    }

    /// The `name` of a builtin installed as a method, static or constructor
    /// (the anonymous and accessor builtins are named by [`Self::name`]).
    fn method_name(self) -> &'static str {
        match self {
            Self::HasOwnProperty => "hasOwnProperty",
            Self::IsPrototypeOf => "isPrototypeOf",
            Self::PropertyIsEnumerable => "propertyIsEnumerable",
            Self::DefineAccessor { set: false } => "__defineGetter__",
            Self::DefineAccessor { set: true } => "__defineSetter__",
            Self::LookupAccessor { set: false } => "__lookupGetter__",
            Self::LookupAccessor { set: true } => "__lookupSetter__",
            Self::ObjectToString
            | Self::ErrorToString
            | Self::ArrayToString
            | Self::StringToString
            | Self::NumberToString
            | Self::BooleanToString
            | Self::FunctionToString => "toString",
            Self::ValueOf | Self::StringValueOf | Self::NumberValueOf | Self::BooleanValueOf => {
                "valueOf"
            }
            Self::Symbol(op) => op.name(),
            Self::DateMethod(op) => op.name(),
            Self::DateGeneric(op) => op.name(),
            Self::CollectionOp { op, .. } => op.name(),
            Self::PromiseThen => "then",
            Self::PromiseCatch => "catch",
            Self::PromiseFinally => "finally",
            Self::GeneratorNext => "next",
            Self::GeneratorReturn => "return",
            Self::GeneratorThrow => "throw",
            Self::AsyncGeneratorMethod(request) => request.name(),
            Self::StringMethod(op) => op.name(),
            Self::StringFromCharCode => "fromCharCode",
            Self::StringFromCodePoint => "fromCodePoint",
            Self::StringRaw => "raw",
            Self::Call | Self::Apply | Self::Bind | Self::FunctionHasInstance => {
                function_name(self)
            }
            Self::Math(op) => math_name(op),
            Self::NumberToFixed => "toFixed",
            Self::NumberToExponential => "toExponential",
            Self::NumberToPrecision => "toPrecision",
            Self::ToLocaleString | Self::NumberToLocaleString | Self::ArrayToLocaleString => {
                "toLocaleString"
            }
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
            Self::ObjectReflect(op) => op.name(),
            Self::IsArray => "isArray",
            Self::ArrayOf => "of",
            Self::ArrayFrom => "from",
            Self::PromiseStatic(op) => op.name(),
            Self::MathSumPrecise => "sumPrecise",
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
            Self::ArrayAt => "at",
            Self::ArrayFind(op) => op.name(),
            Self::ArrayReduceRight => "reduceRight",
            Self::ArrayLastIndexOf => "lastIndexOf",
            Self::ArrayConcat => "concat",
            Self::ArrayFlat { map: false } => "flat",
            Self::ArrayFlat { map: true } => "flatMap",
            Self::ArraySplice => "splice",
            Self::ArraySort => "sort",
            Self::ArrayReverse => "reverse",
            Self::ArrayShift => "shift",
            Self::ArrayUnshift => "unshift",
            Self::ArrayFill => "fill",
            Self::ArrayCopyWithin => "copyWithin",
            Self::ArrayCopy(op) => op.name(),
            Self::IsNaN => "isNaN",
            Self::IsFinite => "isFinite",
            Self::Iter(op) => op.name(),
            _ => "",
        }
    }

    /// The function's `length`.
    pub(super) fn length(self) -> u32 {
        match self {
            Self::FunctionPrototype
            | Self::ThrowTypeError
            | Self::MethodCall
            | Self::BoundCall
            | Self::Protocol(_)
            | Self::AsyncGeneratorDrain
            | Self::AsyncGeneratorStep
            | Self::FunctionToString
            | Self::StringToString
            | Self::StringValueOf
            | Self::NumberValueOf
            | Self::NumberToLocaleString
            | Self::Math(MathOp::Random)
            | Self::BooleanToString
            | Self::BooleanValueOf
            | Self::ObjectToString
            | Self::ValueOf
            | Self::ToLocaleString
            | Self::ErrorToString
            | Self::ArrayToString
            | Self::ArrayToLocaleString
            | Self::DateMethod(_)
            | Self::CollectionSize { .. }
            | Self::ProtoGet
            | Self::CollectionOp {
                op: MapOp::Clear, ..
            }
            | Self::ConsoleLog
            | Self::DateNow
            | Self::ArrayPop
            | Self::ArrayOf
            | Self::ArrayFlat { map: false }
            | Self::ArrayReverse
            | Self::ArrayShift
            | Self::ArrayCopy(ArrayCopyOp::ToReversed)
            | Self::Ctor(
                BuiltinCtor::Map | BuiltinCtor::Set | BuiltinCtor::Symbol | BuiltinCtor::Iterator,
            ) => 0,
            Self::Iter(op) => op.length(),
            Self::Ctor(BuiltinCtor::Date) => 7,
            Self::Math(
                MathOp::Min
                | MathOp::Max
                | MathOp::Pow
                | MathOp::Hypot
                | MathOp::Atan2
                | MathOp::Imul,
            )
            | Self::Ctor(BuiltinCtor::Error(ErrorKind::AggregateError))
            | Self::ParseInt
            | Self::Apply
            | Self::JsonParse
            | Self::ArraySlice
            | Self::ArraySplice
            | Self::ArrayCopyWithin
            | Self::ArrayCopy(ArrayCopyOp::With | ArrayCopyOp::ToSpliced)
            | Self::CollectionOp { op: MapOp::Set, .. }
            | Self::PromiseThen
            | Self::DefineAccessor { .. } => 2,
            Self::StringMethod(op) => op.length(),
            Self::ObjectReflect(op) => op.length(),
            Self::Symbol(op) => op.length(),
            Self::Species(op) => op.length(),
            Self::Iterator(op) => op.length(),
            Self::JsonStringify => 3,
            // Every other builtin takes one argument.
            _ => 1,
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
            | Self::ArrayToString
            | Self::ArrayToLocaleString
            | Self::ArrayAt
            | Self::ArrayFind(_)
            | Self::ArrayReduceRight
            | Self::ArrayLastIndexOf
            | Self::ArrayConcat
            | Self::ArrayFlat { .. }
            | Self::ArraySplice
            | Self::ArraySort
            | Self::ArrayReverse
            | Self::ArrayShift
            | Self::ArrayUnshift
            | Self::ArrayFill
            | Self::ArrayCopyWithin
            | Self::ArrayCopy(_) => Some(BuiltinCtor::Array),
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
            Self::DateMethod(_) | Self::DateGeneric(DateGenericOp::ToJson) => {
                Some(BuiltinCtor::Date)
            }
            Self::Symbol(SymbolOp::ToString | SymbolOp::ValueOf) => Some(BuiltinCtor::Symbol),
            Self::CollectionOp { set, .. } | Self::CollectionForEach { set } => {
                Some(collection(set))
            }
            Self::PromiseThen | Self::PromiseCatch | Self::PromiseFinally => {
                Some(BuiltinCtor::Promise)
            }
            Self::GeneratorNext | Self::GeneratorReturn | Self::GeneratorThrow => {
                return Some(Intrinsic::GeneratorPrototype);
            }
            Self::AsyncGeneratorMethod(_) => return Some(Intrinsic::AsyncGeneratorPrototype),
            Self::Iter(op) => return op.prototype(),
            Self::Iterator(op) => return op.prototype(),
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
    /// A boolean.
    Bool(bool),
    /// Another intrinsic object.
    Intrinsic(Intrinsic),
    /// A well-known symbol: its static `TYPE_SYMBOL` block.
    Symbol(WellKnown),
    /// An accessor property: its static `TYPE_ACCESSOR` block holding the
    /// method blocks of `get` and `set` (the entry has [`abi::ATTR_ACCESSOR`]).
    Accessor {
        /// The getter.
        get: Builtin,
        /// The setter (`undefined` when absent).
        set: Option<Builtin>,
    },
}

/// A registry entry's property key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// An interned string key.
    Str(&'static str),
    /// A well-known symbol key: its static block.
    Symbol(WellKnown),
}

/// One own property of an intrinsic.
#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    /// The intrinsic owning the property.
    pub(super) intrinsic: Intrinsic,
    /// The property key.
    pub(super) key: Key,
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
const GLOBALS: [(&str, u32, Value); 31] = [
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
    (
        "Symbol",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Symbol)),
    ),
    (
        "Iterator",
        METHOD_ATTRS,
        Value::Intrinsic(Intrinsic::Ctor(BuiltinCtor::Iterator)),
    ),
    ("isNaN", METHOD_ATTRS, Value::Method(Builtin::IsNaN)),
    ("isFinite", METHOD_ATTRS, Value::Method(Builtin::IsFinite)),
    (
        "parseFloat",
        METHOD_ATTRS,
        Value::Method(Builtin::ParseFloat),
    ),
    ("parseInt", METHOD_ATTRS, Value::Method(Builtin::ParseInt)),
];

/// The `%Object.prototype%` methods, in slot order.
const OBJECT_PROTOTYPE_METHODS: [Builtin; 10] = [
    Builtin::HasOwnProperty,
    Builtin::ObjectToString,
    Builtin::ValueOf,
    Builtin::IsPrototypeOf,
    Builtin::PropertyIsEnumerable,
    Builtin::ToLocaleString,
    Builtin::DefineAccessor { set: false },
    Builtin::DefineAccessor { set: true },
    Builtin::LookupAccessor { set: false },
    Builtin::LookupAccessor { set: true },
];

/// The registry: every own property of every intrinsic, in slot order per
/// intrinsic.
pub(super) fn registry() -> Vec<Entry> {
    use Intrinsic::{FunctionPrototype, ObjectPrototype};
    let entry = |intrinsic, key, attrs, value| Entry {
        intrinsic,
        key: Key::Str(key),
        attrs,
        value,
    };
    let mut entries: Vec<Entry> = OBJECT_PROTOTYPE_METHODS
        .into_iter()
        .map(|b| entry(ObjectPrototype, b.name(), METHOD_ATTRS, Value::Method(b)))
        .collect();
    let proto = Value::Accessor {
        get: Builtin::ProtoGet,
        set: Some(Builtin::ProtoSet),
    };
    entries.push(entry(ObjectPrototype, "__proto__", ACCESSOR_ATTRS, proto));
    entries.extend([
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
        entry(
            FunctionPrototype,
            "bind",
            METHOD_ATTRS,
            Value::Method(Builtin::Bind),
        ),
        entry(
            FunctionPrototype,
            "toString",
            METHOD_ATTRS,
            Value::Method(Builtin::FunctionToString),
        ),
        entry(FunctionPrototype, "caller", POISON_ATTRS, POISON),
        entry(FunctionPrototype, "arguments", POISON_ATTRS, POISON),
        // Non-writable, non-enumerable, non-configurable: `instanceof` relies
        // on it never changing (the builtin `instanceof` fast paths).
        Entry {
            intrinsic: FunctionPrototype,
            key: Key::Symbol(WellKnown::HasInstance),
            attrs: CONST_ATTRS,
            value: Value::Method(Builtin::FunctionHasInstance),
        },
    ]);
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
            Builtin::Math(_) | Builtin::MathSumPrecise => Intrinsic::Math,
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
        key: Key::Str(key),
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
            BuiltinCtor::Object => &OBJECT_STATICS,
            BuiltinCtor::Array => &[
                Builtin::IsArray,
                Builtin::ArrayOf,
                Builtin::ArrayFrom,
                Builtin::Iter(IterOp::Async(AsyncIterOp::FromAsync(FromAsyncOp::Entry))),
            ],
            BuiltinCtor::String => &[
                Builtin::StringFromCharCode,
                Builtin::StringFromCodePoint,
                Builtin::StringRaw,
            ],
            BuiltinCtor::Promise => &PROMISE_STATICS,
            BuiltinCtor::Date => &[Builtin::DateNow],
            BuiltinCtor::Iterator => &[Builtin::Iterator(IteratorOp::From)],
            BuiltinCtor::Symbol => &[
                Builtin::Symbol(SymbolOp::For),
                Builtin::Symbol(SymbolOp::KeyFor),
            ],
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
        if ctor == BuiltinCtor::Symbol {
            for symbol in WellKnown::ALL {
                let value = Value::Symbol(symbol);
                entries.push(entry(intrinsic, symbol.name(), CONST_ATTRS, value));
            }
        }
        // `%IteratorPrototype%`'s `constructor` is an accessor
        // ([`iterator_prototype_entries`]).
        if ctor != BuiltinCtor::Iterator {
            let ctor = Value::Intrinsic(intrinsic);
            entries.push(entry(prototype, "constructor", METHOD_ATTRS, ctor));
        }
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
    symbol_prototype_entries(entries);
    iterator_entries(entries);
    iterator_prototype_entries(entries);
    well_known_entries(entries);
    species_entries(entries);
}

/// The method names `Array.prototype[@@unscopables]` lists.
const UNSCOPABLES: [&str; 16] = [
    "at",
    "copyWithin",
    "entries",
    "fill",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "includes",
    "keys",
    "toReversed",
    "toSorted",
    "toSpliced",
    "values",
];

/// The `get [@@species]` accessors (configurable only) of `%Array%`,
/// `%Map%`, `%Set%` and `%Promise%`, and `Array.prototype[@@unscopables]`
/// (configurable only) with its `null`-prototype list object (each entry a
/// writable, enumerable, configurable `true`).
fn species_entries(entries: &mut Vec<Entry>) {
    let ctors = [
        BuiltinCtor::Array,
        BuiltinCtor::Map,
        BuiltinCtor::Set,
        BuiltinCtor::Promise,
    ];
    entries.extend(ctors.map(|ctor| Entry {
        intrinsic: Intrinsic::Ctor(ctor),
        key: Key::Symbol(WellKnown::Species),
        attrs: ACCESSOR_ATTRS,
        value: Value::Accessor {
            get: Builtin::Species(SpeciesOp::Getter(ctor)),
            set: None,
        },
    }));
    entries.push(Entry {
        intrinsic: Intrinsic::Prototype(BuiltinCtor::Array),
        key: Key::Symbol(WellKnown::Unscopables),
        attrs: FN_FACT_ATTRS,
        value: Value::Intrinsic(Intrinsic::ArrayUnscopables),
    });
    let all = abi::ATTR_WRITABLE | abi::ATTR_ENUMERABLE | abi::ATTR_CONFIGURABLE;
    entries.extend(UNSCOPABLES.map(|key| Entry {
        intrinsic: Intrinsic::ArrayUnscopables,
        key: Key::Str(key),
        attrs: all,
        value: Value::Bool(true),
    }));
}

/// `%Date.prototype%[@@toPrimitive]` and the builtin `[@@toStringTag]`s
/// (configurable only) `Object.prototype.toString` reads; `%Symbol.prototype%`'s
/// is in [`symbol_prototype_entries`].
fn well_known_entries(entries: &mut Vec<Entry>) {
    entries.push(Entry {
        intrinsic: Intrinsic::Prototype(BuiltinCtor::Date),
        key: Key::Symbol(WellKnown::ToPrimitive),
        attrs: FN_FACT_ATTRS,
        value: Value::Method(Builtin::DateGeneric(DateGenericOp::ToPrimitive)),
    });
    let tags = [
        (Intrinsic::Prototype(BuiltinCtor::Map), "Map"),
        (Intrinsic::Prototype(BuiltinCtor::Set), "Set"),
        (Intrinsic::Prototype(BuiltinCtor::Promise), "Promise"),
        (Intrinsic::GeneratorPrototype, "Generator"),
        (Intrinsic::AsyncGeneratorPrototype, "AsyncGenerator"),
        (Intrinsic::Math, "Math"),
        (Intrinsic::Json, "JSON"),
        (
            Intrinsic::IterPrototype(IterFamily::Array),
            "Array Iterator",
        ),
        (
            Intrinsic::IterPrototype(IterFamily::String),
            "String Iterator",
        ),
        (Intrinsic::IterPrototype(IterFamily::Map), "Map Iterator"),
        (Intrinsic::IterPrototype(IterFamily::Set), "Set Iterator"),
        (Intrinsic::IteratorHelperPrototype, "Iterator Helper"),
    ];
    entries.extend(tags.map(|(intrinsic, tag)| Entry {
        intrinsic,
        key: Key::Symbol(WellKnown::ToStringTag),
        attrs: FN_FACT_ATTRS,
        value: Value::Str(tag),
    }));
}

/// The iteration entries (design/symbol-strings.md §5.4) past the methods
/// [`Builtin::prototype`] installs: the `[@@iterator]` of `%Array.prototype%`
/// (`=== values`), `%Map.prototype%` (`=== entries`), `%Set.prototype%`
/// (`=== values`, as is `keys`) and `%String.prototype%`, and the
/// `[@@iterator]` / `[@@asyncIterator]` returning `this` of
/// `%IteratorPrototype%` / `%AsyncIteratorPrototype%`.
fn iterator_entries(entries: &mut Vec<Entry>) {
    let symbol = |intrinsic, symbol, builtin| Entry {
        intrinsic,
        key: Key::Symbol(symbol),
        attrs: METHOD_ATTRS,
        value: Value::Method(builtin),
    };
    let set_values = Builtin::Iter(IterOp::CollectionIter {
        set: true,
        kind: IterKind::Values,
    });
    entries.extend([
        Entry {
            intrinsic: Intrinsic::Prototype(BuiltinCtor::Set),
            key: Key::Str("keys"),
            attrs: METHOD_ATTRS,
            value: Value::Method(set_values),
        },
        symbol(
            Intrinsic::Prototype(BuiltinCtor::Array),
            WellKnown::Iterator,
            Builtin::Iter(IterOp::ArrayIter(IterKind::Values)),
        ),
        symbol(
            Intrinsic::Prototype(BuiltinCtor::Map),
            WellKnown::Iterator,
            Builtin::Iter(IterOp::CollectionIter {
                set: false,
                kind: IterKind::Entries,
            }),
        ),
        symbol(
            Intrinsic::Prototype(BuiltinCtor::Set),
            WellKnown::Iterator,
            set_values,
        ),
        symbol(
            Intrinsic::Prototype(BuiltinCtor::String),
            WellKnown::Iterator,
            Builtin::Iter(IterOp::StringIterator),
        ),
        symbol(
            Intrinsic::IteratorPrototype,
            WellKnown::Iterator,
            Builtin::Iter(IterOp::IteratorSelf),
        ),
        symbol(
            Intrinsic::AsyncIteratorPrototype,
            WellKnown::AsyncIterator,
            Builtin::Iter(IterOp::AsyncIteratorSelf),
        ),
    ]);
}

/// The `constructor` and `[@@toStringTag]` accessors (configurable only) of
/// `%IteratorPrototype%` (design/symbol-strings.md §5.4): the getters return
/// `%Iterator%` / `"Iterator"`, the setters are
/// `SetterThatIgnoresPrototypeProperties`.
fn iterator_prototype_entries(entries: &mut Vec<Entry>) {
    let accessor = |key, get, set| Entry {
        intrinsic: Intrinsic::IteratorPrototype,
        key,
        attrs: ACCESSOR_ATTRS,
        value: Value::Accessor {
            get: Builtin::Iterator(get),
            set: Some(Builtin::Iterator(set)),
        },
    };
    entries.extend([
        accessor(
            Key::Str("constructor"),
            IteratorOp::ConstructorGet,
            IteratorOp::ConstructorSet,
        ),
        accessor(
            Key::Symbol(WellKnown::ToStringTag),
            IteratorOp::TagGet,
            IteratorOp::TagSet,
        ),
    ]);
}

/// The `%Symbol.prototype%` entries past `constructor` / `toString` /
/// `valueOf`: the `description` getter, `[@@toPrimitive]` and
/// `[@@toStringTag]` (both configurable only).
fn symbol_prototype_entries(entries: &mut Vec<Entry>) {
    let intrinsic = Intrinsic::Prototype(BuiltinCtor::Symbol);
    let description = Value::Accessor {
        get: Builtin::Symbol(SymbolOp::Description),
        set: None,
    };
    entries.extend([
        Entry {
            intrinsic,
            key: Key::Str("description"),
            attrs: ACCESSOR_ATTRS,
            value: description,
        },
        Entry {
            intrinsic,
            key: Key::Symbol(WellKnown::ToPrimitive),
            attrs: FN_FACT_ATTRS,
            value: Value::Method(Builtin::Symbol(SymbolOp::ToPrimitive)),
        },
        Entry {
            intrinsic,
            key: Key::Symbol(WellKnown::ToStringTag),
            attrs: FN_FACT_ATTRS,
            value: Value::Str("Symbol"),
        },
    ]);
}

/// The statics of `Object`, in slot order.
const OBJECT_STATICS: [Builtin; 22] = [
    Builtin::ObjectStatic(ObjectBuiltinOp::Keys),
    Builtin::ObjectStatic(ObjectBuiltinOp::Values),
    Builtin::ObjectStatic(ObjectBuiltinOp::Entries),
    Builtin::ObjectReflect(ReflectOp::ALL[0]),
    Builtin::ObjectReflect(ReflectOp::ALL[1]),
    Builtin::ObjectReflect(ReflectOp::ALL[2]),
    Builtin::ObjectReflect(ReflectOp::ALL[3]),
    Builtin::ObjectReflect(ReflectOp::ALL[4]),
    Builtin::ObjectReflect(ReflectOp::ALL[5]),
    Builtin::ObjectReflect(ReflectOp::ALL[6]),
    Builtin::ObjectReflect(ReflectOp::ALL[7]),
    Builtin::ObjectReflect(ReflectOp::ALL[8]),
    Builtin::ObjectReflect(ReflectOp::ALL[9]),
    Builtin::ObjectReflect(ReflectOp::ALL[10]),
    Builtin::ObjectReflect(ReflectOp::ALL[11]),
    Builtin::ObjectReflect(ReflectOp::ALL[12]),
    Builtin::ObjectReflect(ReflectOp::ALL[13]),
    Builtin::ObjectReflect(ReflectOp::ALL[14]),
    Builtin::ObjectReflect(ReflectOp::ALL[15]),
    Builtin::ObjectReflect(ReflectOp::ALL[16]),
    Builtin::ObjectReflect(ReflectOp::ALL[17]),
    Builtin::ObjectReflect(ReflectOp::ALL[18]),
];

/// The builtin method `intrinsic.key` of the registry, if any.
pub(super) fn method_of(intrinsic: Intrinsic, key: &str) -> Option<Builtin> {
    registry().into_iter().find_map(|e| match e.value {
        Value::Method(builtin)
            if e.intrinsic == intrinsic && matches!(e.key, Key::Str(k) if k == key) =>
        {
            Some(builtin)
        }
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
                | Value::Bool(_)
                | Value::Intrinsic(_)
                | Value::Symbol(_)
                | Value::Accessor { .. } => None,
            };
            let key = match e.key {
                Key::Str(key) => Some(key),
                Key::Symbol(_) => None,
            };
            [key, value]
        })
        .flatten()
        .chain(Builtin::ALL.map(Builtin::name))
        .chain(WellKnown::ALL.map(WellKnown::description))
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

/// The byte offset of the first well-known symbol block from the image base
/// (right after the accessor blocks).
fn symbols_offset(registry: &[Entry]) -> u32 {
    let methods = u32::try_from(methods(registry).count()).unwrap_or(0);
    let accessors = u32::try_from(accessors(registry).count()).unwrap_or(0);
    intrinsics().map(|i| block_size(registry, i)).sum::<u32>()
        + abi::FUNC_BLOCK_SIZE * methods
        + abi::ACCESSOR_BLOCK_SIZE * accessors
}

/// The byte offset of the well-known symbol block of `symbol` from the image
/// base.
fn symbol_offset(registry: &[Entry], symbol: WellKnown) -> u32 {
    symbols_offset(registry) + abi::SYMBOL_BLOCK_SIZE * symbol.index()
}

/// The byte size of the whole image (a multiple of 8, like every block).
pub(super) fn image_size() -> i32 {
    let symbols = u32::try_from(WellKnown::ALL.len()).unwrap_or(0);
    (symbols_offset(&registry()) + abi::SYMBOL_BLOCK_SIZE * symbols).cast_signed()
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

/// The address of `symbol`'s static `TYPE_SYMBOL` block: its property key (a
/// compile-time constant).
pub(super) fn symbol_addr(symbol: WellKnown, helpers: Helpers<'_>) -> i32 {
    (helpers.realm().cast_unsigned() + symbol_offset(&registry(), symbol)).cast_signed()
}

/// The heap-pointer jsval of `builtin`'s static method block (a
/// compile-time constant).
pub(super) fn method_bits(builtin: Builtin, helpers: Helpers<'_>) -> i64 {
    let addr = helpers.realm().cast_unsigned() + method_offset(&registry(), builtin);
    HEAP_BITS | i64::from(addr)
}

/// Build the image bytes for `base`: `first_slot` is the funcref-table slot
/// of [`Builtin::ALL`]'s first entry, `strings` the static-string offsets.
pub(super) fn image(base: i32, first_slot: u32, strings: &StrOffsets) -> Vec<u8> {
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
        let symbol_keys = registry
            .iter()
            .any(|e| e.intrinsic == intrinsic && matches!(e.key, Key::Symbol(_)));
        let tflags = abi::PROPS_HAS_NON_ENUM
            | watched
            | if symbol_keys {
                abi::PROPS_HAS_SYMBOL_KEYS
            } else {
                0
            };
        blob.extend_from_slice(&[abi::TYPE_PROPS, 0, tflags, bit]);
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
                Value::Bool(true) => abi::TRUE,
                Value::Bool(false) => abi::FALSE,
                Value::Intrinsic(i) => heap(base + intrinsic_offset(&registry, i)),
                Value::Symbol(symbol) => heap(base + symbol_offset(&registry, symbol)),
                Value::Accessor { .. } => heap(base + accessor_offset(&registry, i)),
            };
            let key = match e.key {
                Key::Str(key) => at(key),
                Key::Symbol(symbol) => base + symbol_offset(&registry, symbol),
            };
            blob.extend_from_slice(&key.to_le_bytes());
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
    for symbol in WellKnown::ALL {
        blob.extend_from_slice(&[abi::TYPE_SYMBOL, 0, 0, 0]);
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&heap(at(symbol.description())).to_le_bytes());
    }
    blob
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Static-string offsets for every realm string, as `collect_strings`
    /// would assign them.
    fn offsets() -> StrOffsets {
        (0..)
            .zip(strings())
            .map(|(i, s)| (JsStr::from(s), HEAP_BASE + 16 * i))
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
                let expected = match e.key {
                    Key::Str(key) => strings.get(key).copied(),
                    Key::Symbol(symbol) => Some(
                        (base.cast_unsigned() + symbol_offset(&registry, symbol)).cast_signed(),
                    ),
                };
                assert_eq!(Some(key.cast_signed()), expected);
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
            let Key::Str(key @ ("caller" | "arguments")) = e.key else {
                continue;
            };
            let entry = table + abi::PROPS_HEADER_SIZE + abi::PROP_ENTRY_SIZE * k;
            assert_eq!(
                u32_at(&image, entry + abi::PROP_ATTRS_OFFSET),
                abi::ATTR_ACCESSOR | abi::ATTR_CONFIGURABLE,
                "{key}: attrs"
            );
            let block = u32_at(&image, entry + abi::PROP_VALUE_OFFSET) - base.cast_unsigned();
            assert_eq!(image[block as usize], abi::TYPE_ACCESSOR);
            // get === set === the one %ThrowTypeError% for both properties.
            assert_eq!(u64_at(&image, block + abi::ACCESSOR_GET_OFFSET), thrower);
            assert_eq!(u64_at(&image, block + abi::ACCESSOR_SET_OFFSET), thrower);
            found.push(key);
        }
        assert_eq!(found, ["caller", "arguments"]);
        // %ThrowTypeError% is a plain (non-constructor) function: length 0, name "".
        assert_eq!(Builtin::ThrowTypeError.length(), 0);
        assert_eq!(Builtin::ThrowTypeError.name(), "");
        let at = method_offset(&registry, Builtin::ThrowTypeError) as usize;
        assert_eq!(image[at + 2] & abi::FUNC_FLAG_CONSTRUCTOR, 0);
    }

    #[test]
    fn well_known_symbols_are_static_symbol_blocks() {
        let (base, strings) = (4096, offsets());
        let image = image(base, 9, &strings);
        let registry = registry();
        for symbol in WellKnown::ALL {
            let at = symbol_offset(&registry, symbol);
            assert_eq!(at % 8, 0, "{symbol:?}: 8-aligned");
            assert_eq!(
                &image[at as usize..at as usize + 8],
                &[abi::TYPE_SYMBOL, 0, 0, 0, 0, 0, 0, 0]
            );
            let description = u64_at(&image, at + abi::SYMBOL_DESCRIPTION_OFFSET);
            let expected = strings.get(symbol.description()).copied().unwrap_or(0);
            assert_eq!(description, abi::HEAP | u64::from(expected.cast_unsigned()));
            // `Symbol.<name>` holds the block, non-writable/enumerable/configurable.
            let entry = registry
                .iter()
                .find(|e| {
                    e.intrinsic == Intrinsic::Ctor(BuiltinCtor::Symbol)
                        && e.key == Key::Str(symbol.name())
                })
                .map(|e| (e.attrs, e.value));
            assert_eq!(entry, Some((CONST_ATTRS, Value::Symbol(symbol))));
        }
        // Only a table holding a symbol key is flagged HAS_SYMBOL_KEYS.
        for intrinsic in intrinsics() {
            let prefix = if intrinsic.call().is_some() {
                abi::FUNC_BLOCK_SIZE
            } else {
                abi::OBJ_PREFIX_SIZE
            };
            let table = intrinsic_offset(&registry, intrinsic) + prefix;
            let tflags = image[(table + abi::PROPS_TFLAGS_OFFSET) as usize];
            let symbol_keys = matches!(
                intrinsic,
                Intrinsic::Prototype(
                    BuiltinCtor::Symbol
                        | BuiltinCtor::Array
                        | BuiltinCtor::String
                        | BuiltinCtor::Map
                        | BuiltinCtor::Set
                        | BuiltinCtor::Promise
                        | BuiltinCtor::Date
                ) | Intrinsic::Ctor(
                    BuiltinCtor::Array | BuiltinCtor::Map | BuiltinCtor::Set | BuiltinCtor::Promise
                ) | Intrinsic::FunctionPrototype
                    | Intrinsic::IteratorPrototype
                    | Intrinsic::AsyncIteratorPrototype
                    | Intrinsic::IterPrototype(_)
                    | Intrinsic::GeneratorPrototype
                    | Intrinsic::AsyncGeneratorPrototype
                    | Intrinsic::Math
                    | Intrinsic::Json
            );
            assert_eq!(
                tflags & abi::PROPS_HAS_SYMBOL_KEYS != 0,
                symbol_keys,
                "{intrinsic:?}: HAS_SYMBOL_KEYS"
            );
        }
        assert_eq!(
            Intrinsic::Prototype(BuiltinCtor::Symbol).watch_bit(),
            Some(14)
        );
        assert_eq!(Intrinsic::SymbolRegistry.proto(), None);
    }

    #[test]
    fn iterator_prototypes_form_the_spec_chain() {
        use Intrinsic::{
            AsyncGeneratorPrototype, AsyncIteratorPrototype, GeneratorPrototype, IterPrototype,
            IteratorPrototype, ObjectPrototype,
        };
        assert_eq!(IteratorPrototype.proto(), Some(ObjectPrototype));
        assert_eq!(AsyncIteratorPrototype.proto(), Some(ObjectPrototype));
        assert_eq!(GeneratorPrototype.proto(), Some(IteratorPrototype));
        assert_eq!(
            AsyncGeneratorPrototype.proto(),
            Some(AsyncIteratorPrototype)
        );
        // `%AsyncFromSyncIteratorPrototype%` (§5.8): unreachable from the
        // global object, holding the promise-returning `next` / `return` /
        // `throw` of the async-from-sync wrappers.
        assert_eq!(
            Intrinsic::AsyncFromSyncIteratorPrototype.proto(),
            Some(AsyncIteratorPrototype)
        );
        assert_eq!(Intrinsic::AsyncFromSyncIteratorPrototype.watch_bit(), None);
        for request in [
            AsyncGenRequest::Next,
            AsyncGenRequest::Return,
            AsyncGenRequest::Throw,
        ] {
            let builtin = Builtin::Iter(IterOp::Async(AsyncIterOp::FromSync(request)));
            assert_eq!(
                method_of(Intrinsic::AsyncFromSyncIteratorPrototype, request.name()),
                Some(builtin)
            );
            assert_eq!(builtin.length(), 1);
        }
        assert!(
            registry()
                .iter()
                .all(|e| e.value != Value::Intrinsic(Intrinsic::AsyncFromSyncIteratorPrototype))
        );
        for family in IterFamily::ALL {
            assert_eq!(IterPrototype(family).proto(), Some(IteratorPrototype));
            assert!(IterPrototype(family).watch_bit().is_some());
        }
        // `Iterator.prototype` is `%IteratorPrototype%`, whose `constructor`
        // and `@@toStringTag` are accessors; the wrappers inherit it.
        assert_eq!(prototype_of(BuiltinCtor::Iterator), IteratorPrototype);
        assert_eq!(
            Intrinsic::WrapForValidIteratorPrototype.proto(),
            Some(IteratorPrototype)
        );
        let registry = registry();
        let own = |key| {
            registry
                .iter()
                .filter(|e| e.intrinsic == IteratorPrototype && e.key == key)
                .map(|e| (e.attrs, e.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            own(Key::Str("constructor")),
            [(
                ACCESSOR_ATTRS,
                Value::Accessor {
                    get: Builtin::Iterator(IteratorOp::ConstructorGet),
                    set: Some(Builtin::Iterator(IteratorOp::ConstructorSet)),
                }
            )]
        );
        assert_eq!(
            own(Key::Symbol(WellKnown::ToStringTag)),
            [(
                ACCESSOR_ATTRS,
                Value::Accessor {
                    get: Builtin::Iterator(IteratorOp::TagGet),
                    set: Some(Builtin::Iterator(IteratorOp::TagSet)),
                }
            )]
        );
        // The iterator prototypes' bits (15..=19) and the `%Array%` /
        // `%Promise%` ones (20, 21), all distinct.
        let mut bits: Vec<u8> = intrinsics().filter_map(Intrinsic::watch_bit).collect();
        bits.sort_unstable();
        bits.dedup();
        assert_eq!(bits, (0..=21).collect::<Vec<u8>>());
        // The `[@@iterator]` aliases share one method block.
        let symbol_method = |intrinsic, symbol| {
            registry.iter().find_map(|e| {
                (e.intrinsic == intrinsic && e.key == Key::Symbol(symbol)).then_some(e.value)
            })
        };
        assert_eq!(
            symbol_method(
                Intrinsic::Prototype(BuiltinCtor::Array),
                WellKnown::Iterator
            ),
            Some(Value::Method(Builtin::Iter(IterOp::ArrayIter(
                IterKind::Values
            ))))
        );
        let set_values = Builtin::Iter(IterOp::CollectionIter {
            set: true,
            kind: IterKind::Values,
        });
        assert_eq!(
            method_of(Intrinsic::Prototype(BuiltinCtor::Set), "keys"),
            Some(set_values)
        );
        assert_eq!(
            symbol_method(Intrinsic::Prototype(BuiltinCtor::Map), WellKnown::Iterator),
            Some(Value::Method(Builtin::Iter(IterOp::CollectionIter {
                set: false,
                kind: IterKind::Entries,
            })))
        );
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
