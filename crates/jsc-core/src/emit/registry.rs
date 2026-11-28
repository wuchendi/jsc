//! The runtime helper layout: every helper function of an emitted module, in
//! function-index order.
//!
//! The defined functions after `main` and the user functions are a run of
//! [`Group`]s. A group's members are either rows of this file (one
//! `(member, signature, body)` per helper, in the order of the group's enum)
//! or a family module's own `TYPES` and `emit_helpers`. [`declare`] and
//! [`bodies`] walk the same groups, and a helper's index is its group's base
//! plus its position ([`Helpers::core`] ..), so adding a helper is one enum
//! variant and one row.

use ValType::{F64, I32, I64};
use wasm_encoder::{Function, FunctionSection, TypeSection, ValType};

#[allow(clippy::wildcard_imports)]
use super::*;

/// A helper's `(params, results)`.
pub(super) type Sig = (&'static [ValType], &'static [ValType]);

/// What a helper body is emitted from.
#[derive(Clone, Copy)]
pub(super) struct Cx<'a> {
    pub(super) strings: &'a StrOffsets,
    pub(super) helpers: Helpers<'a>,
    pub(super) leaves: &'a LeafSet,
    pub(super) options: CompileOptions,
    pub(super) gc: gc::GcLayout,
}

/// One helper: its member, signature and body.
type Row<M> = (M, Sig, fn(&Cx<'_>) -> Function);

/// The groups of helper functions, in function-index order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    /// [`Core`].
    Core,
    /// The `Number` to string helpers ([`dtoa::emit_helpers`]).
    Dtoa,
    /// [`Tail`].
    Tail,
    /// The two `new Promise` settlers, of the uniform function type.
    Settlers,
    /// [`Ctor`].
    Ctor,
    /// The builtin bodies, in [`Builtin::ALL`] order, of the uniform function
    /// type.
    Builtins,
    /// [`Conv`].
    Conv,
    /// The console renderer and `_start` ([`render::TYPES`]).
    Render,
    /// The Unicode helpers ([`unicode::TYPES`]).
    Unicode,
    /// The `Math` transcendentals ([`libm::TYPES`]).
    Libm,
    /// The promise engine ([`promise_engine::TYPES`]).
    Engine,
    /// The allocator and collector ([`gc::TYPES`]).
    Gc,
    /// [`Extra`].
    Extra,
    /// The binary-data helpers ([`binary::TYPES`]).
    Binary,
    /// [`Big`].
    Bigint,
}

impl Group {
    /// Every group, in function-index order.
    const ALL: [Group; 15] = [
        Self::Core,
        Self::Dtoa,
        Self::Tail,
        Self::Settlers,
        Self::Ctor,
        Self::Builtins,
        Self::Conv,
        Self::Render,
        Self::Unicode,
        Self::Libm,
        Self::Engine,
        Self::Gc,
        Self::Extra,
        Self::Binary,
        Self::Bigint,
    ];

    /// The number of functions in the group.
    fn len(self) -> usize {
        match self {
            Self::Core => CORE.len(),
            Self::Dtoa => DTOA_SIGS.len(),
            Self::Tail => TAIL.len(),
            Self::Settlers => 2,
            Self::Ctor => CTOR.len(),
            Self::Builtins => Builtin::ALL.len(),
            Self::Conv => CONV.len(),
            Self::Render => render::TYPES.len(),
            Self::Unicode => unicode::TYPES.len(),
            Self::Libm => libm::TYPES.len(),
            Self::Engine => promise_engine::TYPES.len(),
            Self::Gc => gc::TYPES.len(),
            Self::Extra => EXTRA.len(),
            Self::Binary => binary::TYPES.len(),
            Self::Bigint => BIGINT.len(),
        }
    }

    /// The group's signatures, or `None` when its functions have the uniform
    /// function type.
    fn sigs(self) -> Option<Vec<Sig>> {
        fn of<M>(rows: &[Row<M>]) -> Vec<Sig> {
            rows.iter().map(|row| row.1).collect()
        }
        Some(match self {
            Self::Core => of(&CORE),
            Self::Dtoa => DTOA_SIGS.to_vec(),
            Self::Tail => of(&TAIL),
            Self::Ctor => of(&CTOR),
            Self::Conv => of(&CONV),
            Self::Render => render::TYPES.to_vec(),
            Self::Unicode => unicode::TYPES.to_vec(),
            Self::Libm => libm::TYPES.to_vec(),
            Self::Engine => promise_engine::TYPES.to_vec(),
            Self::Gc => gc::TYPES.to_vec(),
            Self::Extra => of(&EXTRA),
            Self::Binary => binary::TYPES.to_vec(),
            Self::Bigint => of(&BIGINT),
            Self::Settlers | Self::Builtins => return None,
        })
    }

    /// Hand the group's bodies to `sink` in function-index order, one unit at
    /// a time: a helper of this file, a builtin, or a whole family.
    fn bodies(self, cx: &Cx<'_>, sink: &mut dyn FnMut(Vec<Function>)) {
        fn of<M>(rows: &[Row<M>], cx: &Cx<'_>, sink: &mut dyn FnMut(Vec<Function>)) {
            for row in rows {
                sink(vec![row.2(cx)]);
            }
        }
        let Cx {
            strings,
            helpers,
            leaves,
            options,
            gc,
        } = *cx;
        match self {
            Self::Core => of(&CORE, cx, sink),
            Self::Dtoa => sink(dtoa::emit_helpers(strings, helpers, leaves).into()),
            Self::Tail => of(&TAIL, cx, sink),
            Self::Settlers => sink(vec![
                emit_settler(abi::PROMISE_FULFILLED, helpers, leaves),
                emit_settler(abi::PROMISE_REJECTED, helpers, leaves),
            ]),
            Self::Ctor => of(&CTOR, cx, sink),
            Self::Builtins => {
                for builtin in Builtin::ALL {
                    sink(vec![builtins::emit_builtin(
                        builtin, strings, helpers, leaves,
                    )]);
                }
            }
            Self::Conv => of(&CONV, cx, sink),
            Self::Render => {
                let mut bodies = render::emit_helpers(strings, helpers, leaves);
                bodies.push(emit_start(strings, helpers, options, leaves));
                sink(bodies);
            }
            Self::Unicode => sink(unicode::emit_helpers(helpers, leaves)),
            Self::Libm => sink(libm::emit_helpers(helpers, leaves)),
            Self::Engine => sink(promise_engine::emit_helpers(strings, helpers, leaves)),
            Self::Gc => sink(gc::emit_helpers(helpers, leaves, gc)),
            Self::Extra => of(&EXTRA, cx, sink),
            Self::Binary => sink(binary::emit_helpers(strings, helpers, leaves)),
            Self::Bigint => of(&BIGINT, cx, sink),
        }
    }

    /// The function index of the group's first member, `first` being that of
    /// [`Core::Alloc`], the first helper.
    pub(super) fn base(self, first: u32) -> u32 {
        let before: usize = Self::ALL
            .iter()
            .take_while(|group| **group != self)
            .map(|group| group.len())
            .sum();
        first + u32::try_from(before).unwrap_or(u32::MAX)
    }
}

/// Append every helper's type and function-section entry: one fresh type per
/// helper in function-index order, `uniform` for the settlers and the builtin
/// bodies. The `imports` types follow the settlers; their type indices are
/// returned.
pub(super) fn declare(
    types: &mut TypeSection,
    functions: &mut FunctionSection,
    uniform: u32,
    imports: &[Sig],
) -> Vec<u32> {
    let mut import_types = Vec::new();
    for group in Group::ALL {
        match group.sigs() {
            Some(sigs) => {
                for (params, results) in sigs {
                    functions.function(types.len());
                    types
                        .ty()
                        .function(params.iter().copied(), results.iter().copied());
                }
            }
            None => {
                for _ in 0..group.len() {
                    functions.function(uniform);
                }
            }
        }
        if group == Group::Settlers {
            for (params, results) in imports {
                import_types.push(types.len());
                types
                    .ty()
                    .function(params.iter().copied(), results.iter().copied());
            }
        }
    }
    import_types
}

/// Hand every helper body to `sink` in function-index order, one unit at a
/// time (a single helper, a builtin, or a family emitted together).
pub(super) fn bodies(cx: &Cx<'_>, sink: &mut dyn FnMut(Vec<Function>)) {
    for group in Group::ALL {
        let mut count = 0;
        group.bodies(cx, &mut |unit| {
            count += unit.len();
            sink(unit);
        });
        assert_eq!(count, group.len(), "{group:?} bodies");
    }
}

impl Helpers<'_> {
    /// The function index of the first member of `group`.
    pub(super) fn group(self, group: Group) -> u32 {
        group.base(self.alloc)
    }

    /// The function index of `helper`.
    pub(super) fn core(self, helper: Core) -> u32 {
        self.group(Group::Core) + helper as u32
    }

    /// The function index of `helper`.
    pub(super) fn tail(self, helper: Tail) -> u32 {
        self.group(Group::Tail) + helper as u32
    }

    /// The function index of `helper`.
    pub(super) fn ctor(self, helper: Ctor) -> u32 {
        self.group(Group::Ctor) + helper as u32
    }

    /// The function index of `helper`.
    pub(super) fn conv(self, helper: Conv) -> u32 {
        self.group(Group::Conv) + helper as u32
    }

    /// The function index of `helper`.
    pub(super) fn extra(self, helper: Extra) -> u32 {
        self.group(Group::Extra) + helper as u32
    }

    /// The function index of `helper`.
    pub(super) fn bigint(self, helper: Big) -> u32 {
        self.group(Group::Bigint) + helper as u32
    }
}

/// The helpers from `__alloc` to `__to_number`, in function-index order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Core {
    Alloc,
    Concat,
    StrEq,
    StrCmp,
    StrIndex,
    StrHash,
    InternSlot,
    InternFind,
    Intern,
    IndexValue,
    IndexString,
    StrExoticIndex,
    ToKey,
    PropFind,
    Define,
    FnMaterialize,
    Get,
    Set,
    Has,
    Delete,
    OwnKeys,
    ForInKeys,
    CopyData,
    ArrayReserve,
    ArrayPush,
    ArrayConcat,
    ArraySet,
    ArraySetLength,
    SparseGet,
    SparseSet,
    ProtosIndexless,
    ProtoElem,
    ProtoSet,
    ArrayDefine,
    SparseDefine,
    JsonEscape,
    JsonStr,
    SameValueZero,
    ArraySearch,
    MapFind,
    MapSet,
    MapGet,
    MapHas,
    MapDelete,
    LooseEq,
    StrToArray,
    JsonWs,
    JsonThrow,
    JsonString,
    JsonNumber,
    JsonValue,
    JsonArray,
    JsonObject,
    JsonParse,
    GenToArray,
    NullishThrow,
    PropThrow,
    BoxF64,
    ToNumber,
}

/// The number parsers, the clock and the trampolines, after the `dtoa` helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tail {
    NumUnit,
    StrWs,
    DecParse,
    DecSlow,
    DecRound,
    DecLeft,
    DecRight,
    DigitSpan,
    RadixPow2,
    RadixFold,
    StringToNumber,
    ToNumberPrim,
    ParseFloat,
    ParseInt,
    Now,
    Random,
    Invoke1,
    AsyncStep,
}

/// The block constructors and `__has_own`, after the settlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ctor {
    NewString,
    NewArray,
    HasOwn,
}

/// The conversion, descriptor, `super` and private-name helpers, after the builtin bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Conv {
    ToPrimitive,
    ToStr,
    Add,
    Compare,
    ArrayJoin,
    ToObject,
    JsonRep,
    JsonIndent,
    JsonRevive,
    DefineOwn,
    ToDesc,
    OwnDesc,
    SetIntegrity,
    TestIntegrity,
    OwnEnum,
    SetProto,
    PromiseSelfError,
    SymbolGet,
    SuperGet,
    SuperSet,
    PrivateFind,
    PrivateAdd,
    PrivateGet,
    PrivateSet,
    PrivateBrand,
    PrivateIn,
}

/// The helpers after the collector's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Extra {
    StackOverflow,
    MapHash,
    MapReindex,
}

/// The `BigInt` runtime ([`bigint`]), after the extra helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Big {
    New,
    Trim,
    Eq,
    CmpMag,
    Cmp,
    CmpNum,
    Hash,
    ToString,
    FromString,
    ToF64,
    FromF64,
    ToNumeric,
    ToBigInt,
    LooseEq,
    NumberValue,
    AsN,
    ToI64,
    FromI64,
    Add,
    Sub,
    Mul,
    DivMod,
    Pow,
    Neg,
    MagStep,
    AddSmall,
    Not,
    BitOp,
    Shift,
    NumericOp,
    NumericUnary,
}

const CORE: [Row<Core>; 59] = [
    (Core::Alloc, (&[I32], &[I32]), |cx| {
        emit_alloc(cx.helpers, cx.leaves, cx.options.gc_stress, false)
    }),
    (Core::Concat, (&[I32, I32], &[I32]), |cx| {
        emit_concat(cx.helpers.alloc, cx.leaves)
    }),
    (Core::StrEq, (&[I32, I32], &[I32]), |cx| {
        emit_str_eq(cx.leaves)
    }),
    (Core::StrCmp, (&[I32, I32], &[I32]), |cx| {
        emit_str_cmp(cx.leaves)
    }),
    (Core::StrIndex, (&[I32, I32], &[I32]), |cx| {
        emit_str_index(cx.helpers.alloc, cx.leaves)
    }),
    (Core::StrHash, (&[I32], &[I32]), |cx| {
        emit_str_hash(cx.leaves)
    }),
    (Core::InternSlot, (&[I32], &[I32]), |cx| {
        emit_intern_slot(cx.helpers, cx.leaves)
    }),
    (Core::InternFind, (&[I32], &[I32]), |cx| {
        emit_intern_find(cx.helpers, cx.leaves)
    }),
    (Core::Intern, (&[I32], &[I32]), |cx| {
        emit_intern(cx.helpers, cx.leaves)
    }),
    (Core::IndexValue, (&[I32], &[I32]), |cx| {
        emit_index_value(cx.leaves)
    }),
    (Core::IndexString, (&[I32], &[I32]), |cx| {
        emit_index_string(cx.helpers, cx.leaves)
    }),
    (Core::StrExoticIndex, (&[I32, I32], &[I32]), |cx| {
        emit_str_exotic_index(cx.helpers, cx.leaves)
    }),
    (Core::ToKey, (&[I64, I32], &[I32]), |cx| {
        emit_to_key(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::PropFind, (&[I32, I32], &[I32]), |cx| {
        emit_prop_find(cx.helpers, cx.leaves)
    }),
    (Core::Define, (&[I32, I32, I64, I32], &[]), |cx| {
        emit_define(cx.helpers, cx.leaves)
    }),
    (Core::FnMaterialize, (&[I32], &[]), |cx| {
        emit_fn_materialize(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::Get, (&[I64, I32], &[I64]), |cx| {
        emit_get(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::Set, (&[I64, I32, I64], &[]), |cx| {
        emit_set(cx.helpers, cx.leaves)
    }),
    (Core::Has, (&[I32, I64], &[I32]), |cx| {
        emit_has(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::Delete, (&[I64, I32], &[]), |cx| {
        emit_delete(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::OwnKeys, (&[I64, I32], &[I64]), |cx| {
        emit_own_keys(cx.helpers, cx.leaves)
    }),
    (Core::ForInKeys, (&[I64], &[I64]), |cx| {
        emit_for_in_keys(cx.helpers, cx.leaves)
    }),
    (Core::CopyData, (&[I64, I64, I64], &[I64]), |cx| {
        emit_copy_data(cx.helpers, cx.leaves)
    }),
    (Core::ArrayReserve, (&[I32, I32], &[]), |cx| {
        emit_array_reserve(cx.helpers.alloc, cx.leaves)
    }),
    (Core::ArrayPush, (&[I32, I64], &[I32]), |cx| {
        emit_array_push(
            cx.helpers.array_reserve(),
            cx.helpers.sparse_define(),
            cx.leaves,
        )
    }),
    (Core::ArrayConcat, (&[I32, I32], &[I32]), |cx| {
        emit_array_concat(
            cx.helpers.array_reserve(),
            cx.helpers.sparse_get(),
            cx.helpers.proto_elem(),
            cx.leaves,
        )
    }),
    (Core::ArraySet, (&[I64, I64, I64], &[]), |cx| {
        emit_array_set(cx.strings, cx.helpers, false, cx.leaves)
    }),
    (Core::ArraySetLength, (&[I32, I64], &[]), |cx| {
        emit_array_set_length(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::SparseGet, (&[I32, I32], &[I64]), |cx| {
        emit_sparse_get(cx.helpers, cx.leaves)
    }),
    (Core::SparseSet, (&[I32, I32, I64], &[]), |cx| {
        emit_sparse_set(cx.helpers, false, cx.leaves)
    }),
    (Core::ProtosIndexless, (&[I32], &[I32]), |cx| {
        emit_protos_indexless(cx.leaves)
    }),
    (Core::ProtoElem, (&[I32, I32], &[I64]), |cx| {
        emit_proto_elem(cx.helpers, cx.leaves)
    }),
    (Core::ProtoSet, (&[I32, I32, I64], &[I32]), |cx| {
        emit_proto_set(cx.helpers, cx.leaves)
    }),
    (Core::ArrayDefine, (&[I64, I64, I64], &[]), |cx| {
        emit_array_set(cx.strings, cx.helpers, true, cx.leaves)
    }),
    (Core::SparseDefine, (&[I32, I32, I64], &[]), |cx| {
        emit_sparse_set(cx.helpers, true, cx.leaves)
    }),
    (Core::JsonEscape, (&[I32], &[I32]), |cx| {
        emit_json_escape(cx.helpers.alloc, cx.leaves)
    }),
    (Core::JsonStr, (&[I64], &[I32]), |cx| {
        emit_json_str(
            cx.helpers.num_to_str(),
            cx.helpers.json_escape,
            JsonTokens::resolve(cx.strings),
            cx.leaves,
        )
    }),
    (Core::SameValueZero, (&[I64, I64], &[I32]), |cx| {
        emit_same_value_zero(cx.helpers, cx.leaves)
    }),
    (Core::ArraySearch, (&[I64, I64, I32], &[I64]), |cx| {
        builtins::emit_array_search(cx.helpers, cx.leaves)
    }),
    (Core::MapFind, (&[I32, I64], &[I32]), |cx| {
        emit_map_find(cx.helpers, cx.leaves)
    }),
    (Core::MapSet, (&[I32, I64, I64], &[]), |cx| {
        emit_map_set(cx.helpers, cx.leaves)
    }),
    (Core::MapGet, (&[I32, I64], &[I64]), |cx| {
        emit_map_get(cx.helpers.map_find, cx.leaves)
    }),
    (Core::MapHas, (&[I32, I64], &[I32]), |cx| {
        emit_map_has(cx.helpers.map_find, cx.leaves)
    }),
    (Core::MapDelete, (&[I32, I64], &[I32]), |cx| {
        emit_map_delete(cx.helpers.map_find, cx.leaves)
    }),
    (Core::LooseEq, (&[I64, I64], &[I32]), |cx| {
        emit_loose_eq(cx.helpers, cx.leaves)
    }),
    (Core::StrToArray, (&[I64], &[I64]), |cx| {
        emit_str_to_array(
            cx.helpers.new_array(),
            cx.helpers.str_index,
            cx.helpers.alloc,
            cx.leaves,
        )
    }),
    (Core::JsonWs, (&[], &[]), |cx| emit_json_ws(cx.leaves)),
    (Core::JsonThrow, (&[], &[I64]), |cx| {
        emit_json_throw(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::JsonString, (&[], &[I64]), |cx| {
        emit_json_string(cx.helpers.alloc, cx.helpers, cx.leaves)
    }),
    (Core::JsonNumber, (&[], &[I64]), |cx| {
        emit_json_number(cx.helpers, cx.leaves)
    }),
    (Core::JsonValue, (&[], &[I64]), |cx| {
        emit_json_value(cx.helpers, cx.leaves)
    }),
    (Core::JsonArray, (&[], &[I64]), |cx| {
        emit_json_array(cx.helpers, cx.leaves)
    }),
    (Core::JsonObject, (&[], &[I64]), |cx| {
        emit_json_object(cx.helpers, cx.leaves)
    }),
    (Core::JsonParse, (&[I64], &[I64]), |cx| {
        emit_json_parse(cx.helpers, cx.leaves)
    }),
    (Core::GenToArray, (&[I64], &[I64]), |cx| {
        generator::emit_gen_to_array(cx.helpers, cx.leaves)
    }),
    (Core::NullishThrow, (&[I64, I64, I32], &[]), |cx| {
        emit_nullish_throw(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::PropThrow, (&[I64, I64, I32], &[]), |cx| {
        emit_prop_throw(cx.strings, cx.helpers, cx.leaves)
    }),
    (Core::BoxF64, (&[F64], &[I64]), |cx| {
        emit_box_f64_helper(cx.leaves)
    }),
    (Core::ToNumber, (&[I64], &[I64]), |cx| {
        emit_to_number_helper(cx.strings, cx.helpers, cx.leaves)
    }),
];

const DTOA_SIGS: [Sig; 12] = [
    (&[I64, I64], &[I64, I64]),
    (&[I32, I32], &[I64, I64]),
    (&[I64, I64, I64, I32], &[I64]),
    (&[I64, I32], &[I32]),
    (&[I64], &[I64, I32]),
    (&[I32, I32], &[I32]),
    (&[I64], &[I32]),
    (&[I64], &[I32, I32, I32]),
    (&[I64, I32, I32], &[I32]),
    (&[I64, I32], &[I32]),
    (&[I64], &[I32]),
    (&[I64], &[I32]),
];

const TAIL: [Row<Tail>; 18] = [
    (Tail::NumUnit, (&[I32, I32], &[I32]), |cx| {
        strtod::emit_num_unit(cx.leaves)
    }),
    (Tail::StrWs, (&[I32], &[I32]), |cx| {
        strtod::emit_str_ws(cx.leaves)
    }),
    (Tail::DecParse, (&[I32, I32, I32], &[F64]), |cx| {
        strtod::emit_dec_parse(cx.helpers, cx.leaves)
    }),
    (Tail::DecSlow, (&[], &[I64]), |cx| {
        strtod::emit_dec_slow(cx.helpers, cx.leaves)
    }),
    (Tail::DecRound, (&[], &[I64]), |cx| {
        strtod::emit_dec_round(cx.leaves)
    }),
    (Tail::DecLeft, (&[I32], &[]), |cx| {
        strtod::emit_dec_left(cx.leaves)
    }),
    (Tail::DecRight, (&[I32], &[]), |cx| {
        strtod::emit_dec_right(cx.leaves)
    }),
    (Tail::DigitSpan, (&[I32, I32, I32, I32], &[I32]), |cx| {
        strtod::emit_digit_span(cx.helpers, cx.leaves)
    }),
    (Tail::RadixPow2, (&[I32, I32, I32, I32], &[F64]), |cx| {
        strtod::emit_radix_pow2(cx.helpers, cx.leaves)
    }),
    (Tail::RadixFold, (&[I32, I32, I32, I32], &[F64]), |cx| {
        strtod::emit_radix_fold(cx.helpers, cx.leaves)
    }),
    (Tail::StringToNumber, (&[I32], &[F64]), |cx| {
        strtod::emit_string_to_number(cx.helpers, cx.leaves)
    }),
    (Tail::ToNumberPrim, (&[I64], &[I64]), |cx| {
        strtod::emit_to_number_prim(cx.helpers, cx.leaves)
    }),
    (Tail::ParseFloat, (&[I64], &[I64]), |cx| {
        strtod::emit_parse_float(cx.helpers, cx.leaves)
    }),
    (Tail::ParseInt, (&[I64, I32], &[I64]), |cx| {
        strtod::emit_parse_int(cx.helpers, cx.leaves)
    }),
    (Tail::Now, (&[], &[F64]), |cx| numeric::emit_now(cx.leaves)),
    (Tail::Random, (&[], &[F64]), |cx| {
        numeric::emit_random(cx.leaves)
    }),
    (Tail::Invoke1, (&[I64, I64], &[I64]), |cx| {
        emit_invoke1(cx.helpers, cx.leaves)
    }),
    (Tail::AsyncStep, (&[I64, I64], &[I64]), |cx| {
        generator::emit_async_step(cx.helpers, cx.leaves)
    }),
];

const CTOR: [Row<Ctor>; 3] = [
    (Ctor::NewString, (&[I32, I32], &[I32]), |cx| {
        emit_new_string(cx.helpers.alloc, cx.leaves)
    }),
    (Ctor::NewArray, (&[I32], &[I32]), |cx| {
        emit_new_array(
            cx.helpers,
            intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Array), cx.helpers),
            cx.leaves,
        )
    }),
    (Ctor::HasOwn, (&[I32, I64], &[I32]), |cx| {
        emit_has_own(cx.strings, cx.helpers, cx.leaves)
    }),
];

const CONV: [Row<Conv>; 26] = [
    (Conv::ToPrimitive, (&[I64, I32], &[I64]), |cx| {
        emit_to_primitive(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::ToStr, (&[I64], &[I32]), |cx| {
        convert::emit_to_string(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::Add, (&[I64, I64], &[I64]), |cx| {
        emit_add_helper(cx.helpers, cx.leaves)
    }),
    (Conv::Compare, (&[I64, I64, I32], &[I32]), |cx| {
        emit_compare(cx.helpers, cx.leaves)
    }),
    (Conv::ArrayJoin, (&[I64, I64], &[I64]), |cx| {
        emit_array_join(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::ToObject, (&[I64], &[I64]), |cx| {
        builtins::emit_to_object(cx.strings, cx.helpers, cx.leaves)
    }),
    (
        Conv::JsonRep,
        (&[I64, I64, I64, I64, I64, I32], &[I64]),
        |cx| builtins::emit_json_rep(cx.strings, cx.helpers, cx.leaves),
    ),
    (Conv::JsonIndent, (&[I32, I32], &[I32]), |cx| {
        builtins::emit_json_indent(cx.helpers, cx.leaves)
    }),
    (Conv::JsonRevive, (&[I64, I32, I64], &[I64]), |cx| {
        builtins::emit_json_revive(cx.helpers, cx.leaves)
    }),
    (
        Conv::DefineOwn,
        (&[I32, I32, I32, I64, I64, I64], &[I32]),
        |cx| emit_define_own(cx.strings, cx.helpers, cx.leaves),
    ),
    (Conv::ToDesc, (&[I64, I32], &[]), |cx| {
        emit_to_desc(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::OwnDesc, (&[I64, I32], &[I64]), |cx| {
        emit_own_desc(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::SetIntegrity, (&[I64, I32], &[]), |cx| {
        emit_set_integrity(cx.helpers, cx.leaves)
    }),
    (Conv::TestIntegrity, (&[I64, I32], &[I32]), |cx| {
        emit_test_integrity(cx.leaves)
    }),
    (Conv::OwnEnum, (&[I64, I32], &[I32]), |cx| {
        emit_own_enum(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::SetProto, (&[I32, I64], &[I32]), |cx| {
        emit_set_proto(cx.helpers, cx.leaves)
    }),
    (Conv::PromiseSelfError, (&[], &[I64]), |cx| {
        builtins::emit_promise_self_error(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::SymbolGet, (&[I64, I32], &[I64]), |cx| {
        emit_symbol_get(cx.helpers, cx.leaves)
    }),
    (Conv::SuperGet, (&[I64, I32, I64], &[I64]), |cx| {
        emit_super_get(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::SuperSet, (&[I64, I32, I64, I64], &[]), |cx| {
        emit_super_set(cx.strings, cx.helpers, cx.leaves)
    }),
    (Conv::PrivateFind, (&[I64, I32], &[I32]), |cx| {
        emit_private_find(cx.helpers, cx.leaves)
    }),
    (Conv::PrivateAdd, (&[I64, I32, I64, I32, I32], &[]), |cx| {
        emit_private_add(cx.helpers, cx.leaves)
    }),
    (Conv::PrivateGet, (&[I64, I32, I32], &[I64]), |cx| {
        emit_private_get(cx.helpers, cx.leaves)
    }),
    (Conv::PrivateSet, (&[I64, I32, I64, I32], &[I64]), |cx| {
        emit_private_set(cx.helpers, cx.leaves)
    }),
    (
        Conv::PrivateBrand,
        (&[I64, I64, I64, I32, I32], &[I64]),
        |cx| emit_private_brand(cx.helpers, cx.leaves),
    ),
    (Conv::PrivateIn, (&[I64, I64, I32], &[I64]), |cx| {
        emit_private_in(cx.strings, cx.helpers, cx.leaves)
    }),
];

const EXTRA: [Row<Extra>; 3] = [
    (Extra::StackOverflow, (&[], &[]), |cx| {
        emit_stack_overflow(cx.strings, cx.helpers, cx.leaves)
    }),
    (Extra::MapHash, (&[I64], &[I32]), |cx| {
        emit_map_hash(cx.helpers, cx.leaves)
    }),
    (Extra::MapReindex, (&[I32], &[]), |cx| {
        emit_map_reindex(cx.helpers, cx.leaves)
    }),
];

const BIGINT: [Row<Big>; 31] = [
    (Big::New, (&[I32, I32], &[I32]), |cx| {
        bigint::emit_new(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::Trim, (&[I32], &[I64]), |cx| {
        bigint::emit_trim(cx.leaves)
    }),
    (Big::Eq, (&[I64, I64], &[I32]), |cx| {
        bigint::emit_eq(cx.leaves)
    }),
    (Big::CmpMag, (&[I32, I32], &[I32]), |cx| {
        bigint::emit_cmp_mag(cx.leaves)
    }),
    (Big::Cmp, (&[I32, I32], &[I32]), |cx| {
        bigint::emit_cmp(cx.helpers, cx.leaves)
    }),
    (Big::CmpNum, (&[I32, F64], &[I32]), |cx| {
        bigint::emit_cmp_num(cx.leaves)
    }),
    (Big::Hash, (&[I32], &[I32]), |cx| {
        bigint::emit_hash(cx.leaves)
    }),
    (Big::ToString, (&[I64, I32], &[I32]), |cx| {
        bigint::emit_to_string(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::FromString, (&[I32], &[I64]), |cx| {
        bigint::emit_from_string(cx.helpers, cx.leaves)
    }),
    (Big::ToF64, (&[I32], &[F64]), |cx| {
        bigint::emit_to_f64(cx.leaves)
    }),
    (Big::FromF64, (&[F64], &[I64]), |cx| {
        bigint::emit_from_f64(cx.helpers, cx.leaves)
    }),
    (Big::ToNumeric, (&[I64], &[I64]), |cx| {
        bigint::emit_to_numeric(cx.helpers, cx.leaves)
    }),
    (Big::ToBigInt, (&[I64], &[I64]), |cx| {
        bigint::emit_to_bigint(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::LooseEq, (&[I64, I64], &[I32]), |cx| {
        bigint::emit_loose_eq(cx.helpers, cx.leaves)
    }),
    (Big::NumberValue, (&[I64], &[I64]), |cx| {
        bigint::emit_number_value(cx.helpers, cx.leaves)
    }),
    (Big::AsN, (&[F64, I64, I32], &[I64]), |cx| {
        bigint::emit_as_n(cx.helpers, cx.leaves)
    }),
    (Big::ToI64, (&[I32], &[I64]), |cx| {
        bigint::emit_to_i64(cx.leaves)
    }),
    (Big::FromI64, (&[I64, I32], &[I64]), |cx| {
        bigint::emit_from_i64(cx.helpers, cx.leaves)
    }),
    (Big::Add, (&[I32, I32], &[I64]), |cx| {
        bigint::emit_add_sub(false, cx.helpers, cx.leaves)
    }),
    (Big::Sub, (&[I32, I32], &[I64]), |cx| {
        bigint::emit_add_sub(true, cx.helpers, cx.leaves)
    }),
    (Big::Mul, (&[I32, I32], &[I64]), |cx| {
        bigint::emit_mul(cx.helpers, cx.leaves)
    }),
    (Big::DivMod, (&[I32, I32, I32], &[I64]), |cx| {
        bigint::emit_divmod(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::Pow, (&[I32, I32], &[I64]), |cx| {
        bigint::emit_pow(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::Neg, (&[I32], &[I64]), |cx| {
        bigint::emit_neg(cx.helpers, cx.leaves)
    }),
    (Big::MagStep, (&[I32, I32, I32], &[I64]), |cx| {
        bigint::emit_mag_step(cx.helpers, cx.leaves)
    }),
    (Big::AddSmall, (&[I64, I32], &[I64]), |cx| {
        bigint::emit_add_small(cx.helpers, cx.leaves)
    }),
    (Big::Not, (&[I32], &[I64]), |cx| {
        bigint::emit_not(cx.helpers, cx.leaves)
    }),
    (Big::BitOp, (&[I32, I32, I32], &[I64]), |cx| {
        bigint::emit_bitop(cx.helpers, cx.leaves)
    }),
    (Big::Shift, (&[I32, I32, I32], &[I64]), |cx| {
        bigint::emit_shift(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::NumericOp, (&[I64, I64, I32], &[I64]), |cx| {
        bigint::emit_numeric_op(cx.strings, cx.helpers, cx.leaves)
    }),
    (Big::NumericUnary, (&[I64, I32], &[I64]), |cx| {
        bigint::emit_numeric_unary(cx.helpers, cx.leaves)
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's position is its member's discriminant.
    #[test]
    fn rows_are_in_member_order() {
        fn check<M: Copy + std::fmt::Debug>(rows: &[Row<M>], index: fn(M) -> usize) {
            for (position, row) in rows.iter().enumerate() {
                assert_eq!(index(row.0), position, "{:?}", row.0);
            }
        }
        check(&CORE, |member| member as usize);
        check(&TAIL, |member| member as usize);
        check(&CTOR, |member| member as usize);
        check(&CONV, |member| member as usize);
        check(&EXTRA, |member| member as usize);
        check(&BIGINT, |member| member as usize);
    }
}
