//! The realm image (design 4.1/4.2): every intrinsic object is a static block
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
//! ([`super::builtins`]). A static table that a user mutation overflows is
//! rebuilt on the heap by `__define` like any other.

#[allow(clippy::wildcard_imports)]
use super::*;

/// An intrinsic object of the realm image. (The constructors join them in
/// S4 T3.)
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
}

impl From<GlobalIntrinsic> for Intrinsic {
    fn from(global: GlobalIntrinsic) -> Self {
        match global {
            GlobalIntrinsic::Global => Self::Global,
            GlobalIntrinsic::Math => Self::Math,
            GlobalIntrinsic::Json => Self::Json,
            GlobalIntrinsic::Console => Self::Console,
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
            | Self::Console => Some(Self::ObjectPrototype),
            Self::ErrorPrototype(_) => Some(Self::ErrorPrototype(ErrorKind::Error)),
        }
    }

    /// The builtin body a callable intrinsic runs, or `None` for an ordinary
    /// object.
    fn call(self) -> Option<Builtin> {
        (self == Self::FunctionPrototype).then_some(Builtin::FunctionPrototype)
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
}

impl Builtin {
    /// Every builtin, in funcref-table (and `__fn_info`) order.
    pub(super) const ALL: [Builtin; 19] = [
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
        Self::JsonStringify,
        Self::JsonParse,
        Self::ConsoleLog,
    ];

    /// The function's `name`.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::FunctionPrototype => "",
            Self::HasOwnProperty => "hasOwnProperty",
            Self::ObjectToString | Self::ErrorToString => "toString",
            Self::ValueOf => "valueOf",
            Self::Call => "call",
            Self::Apply => "apply",
            Self::Math(op) => match op {
                MathOp::Floor => "floor",
                MathOp::Ceil => "ceil",
                MathOp::Round => "round",
                MathOp::Trunc => "trunc",
                MathOp::Abs => "abs",
                MathOp::Sqrt => "sqrt",
                MathOp::Min => "min",
                MathOp::Max => "max",
                MathOp::Pow => "pow",
            },
            Self::JsonStringify => "stringify",
            Self::JsonParse => "parse",
            Self::ConsoleLog => "log",
        }
    }

    /// The function's `length`.
    pub(super) fn length(self) -> u32 {
        match self {
            Self::FunctionPrototype
            | Self::ObjectToString
            | Self::ValueOf
            | Self::ErrorToString
            | Self::ConsoleLog => 0,
            Self::Math(MathOp::Min | MathOp::Max | MathOp::Pow) | Self::Apply | Self::JsonParse => {
                2
            }
            Self::HasOwnProperty | Self::Call | Self::Math(_) => 1,
            Self::JsonStringify => 3,
        }
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

/// The global object's own properties, one line per global binding (design
/// 4.2).
const GLOBALS: [(&str, u32, Value); 7] = [
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
];

/// The registry: every own property of every intrinsic, in slot order per
/// intrinsic. `%Object.prototype%.constructor` and
/// `%Function.prototype%.constructor` are added with the constructors (T3).
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
    entries.extend(GLOBALS.map(|(key, attrs, value)| entry(Intrinsic::Global, key, attrs, value)));
    entries
}

/// Every static string the image references (keys, string values, builtin
/// names). Interned in every program.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    registry()
        .into_iter()
        .flat_map(|e| {
            let value = match e.value {
                Value::Str(s) => Some(s),
                Value::Method(_) | Value::Num(_) | Value::Undefined | Value::Intrinsic(_) => None,
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

/// The builtins with a static method block, in image order.
fn methods(registry: &[Entry]) -> impl Iterator<Item = Builtin> + '_ {
    Builtin::ALL
        .into_iter()
        .filter(|b| registry.iter().any(|e| e.value == Value::Method(*b)))
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

/// The byte size of the whole image (a multiple of 8, like every block).
pub(super) fn image_size() -> i32 {
    let registry = registry();
    let methods = u32::try_from(methods(&registry).count()).unwrap_or(0);
    let size = intrinsics().map(|i| block_size(&registry, i)).sum::<u32>()
        + abi::FUNC_BLOCK_SIZE * methods;
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
            Some(_) => ([abi::TYPE_FUNCTION, 0, 0, 0], abi::FUNC_BLOCK_SIZE),
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
        blob.extend_from_slice(&[abi::TYPE_PROPS, 0, abi::PROPS_HAS_NON_ENUM, 0]);
        for field in [count, count, count] {
            blob.extend_from_slice(&field.to_le_bytes());
        }
        for e in registry.iter().filter(|e| e.intrinsic == intrinsic) {
            let value = match e.value {
                Value::Method(b) => heap(base + method_offset(&registry, b)),
                Value::Str(s) => heap(at(s)),
                Value::Num(n) if n.is_nan() => abi::QNAN,
                Value::Num(n) => n.to_bits(),
                Value::Undefined => abi::UNDEFINED,
                Value::Intrinsic(i) => heap(base + intrinsic_offset(&registry, i)),
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
