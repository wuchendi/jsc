//! In-house intermediate representation for the numeric-program subset.
//!
//! The compiler works over a tiny, f64-only slice of JavaScript. [`IrNode`] is
//! the expression tree the frontend lowers into and the backend later walks;
//! [`IrProgram`] wraps a sequence of [`IrStmt`] statements (variable
//! declarations and expression statements) plus the count of `f64` locals the
//! function needs. These are pure data types with no parsing or codegen
//! behaviour of their own.

/// The flavour of an [`IrNode::IndexOpSet`] read-modify-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpSetKind {
    /// `obj[index] op= rhs`: combine the current value as-is; yield the new value.
    Compound,
    /// `++obj[index]` / `--obj[index]`: convert the current value with `ToNumber`
    /// before combining; yield the new value.
    PrefixUpdate,
    /// `obj[index]++` / `obj[index]--`: like [`Self::PrefixUpdate`], but yield the
    /// OLD value converted to a number.
    PostfixUpdate,
}

/// A binary operator over jsval operands.
///
/// Arithmetic operators (`Add`..`Mod`) unbox their operands to `f64` and produce
/// a number jsval; comparison operators (`Lt`..`LooseNe`) produce a boolean jsval
/// (`true`/`false`). Relational operators (`Lt`..`Ge`) compare numerically;
/// `Eq`/`Ne` are strict `===`/`!==` (type-then-value across all jsval types);
/// `LooseEq`/`LooseNe` are loose `==`/`!=` (Abstract Equality with coercion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Addition (`+`).
    Add,
    /// Subtraction (`-`).
    Sub,
    /// Multiplication (`*`).
    Mul,
    /// Division (`/`).
    Div,
    /// Remainder (`%`): the `f64` truncated remainder `a - b * trunc(a / b)`,
    /// matching JS `%` sign semantics.
    Mod,
    /// Less-than (`<`).
    Lt,
    /// Greater-than (`>`).
    Gt,
    /// Less-than-or-equal (`<=`).
    Le,
    /// Greater-than-or-equal (`>=`).
    Ge,
    /// Strict equality (`===`).
    Eq,
    /// Strict inequality (`!==`).
    Ne,
    /// Loose equality (`==`): JS Abstract Equality with number/string/boolean
    /// coercion (`null`/`undefined` mutually equal), falling back to identity for
    /// objects. See `emit_loose_eq`.
    LooseEq,
    /// Loose inequality (`!=`): the negation of [`BinOp::LooseEq`].
    LooseNe,
}

/// A short-circuit logical operator. Unlike [`BinOp`], these evaluate the left
/// operand once and branch, yielding an **operand jsval** (not a coerced
/// boolean): `&&`/`||` branch on JS truthiness, `??` branches on nullishness
/// (`null`/`undefined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    /// Logical AND (`&&`): if the left is truthy, yield the right, else the left.
    And,
    /// Logical OR (`||`): if the left is truthy, yield the left, else the right.
    Or,
    /// Nullish coalescing (`??`): if the left is `null`/`undefined`, yield the
    /// right, else the left.
    Nullish,
}

/// A `Math.*` built-in method. `Math` is not a real binding — these are
/// recognized syntactically from a `Math.<name>(..)` member callee at codegen and
/// lowered to native wasm `f64` ops (or an inline loop, for [`MathOp::Pow`]). The
/// `Math.PI` / `Math.E` constants are property READS and lower directly to
/// [`IrNode::Num`], so they need no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathOp {
    /// `Math.floor(x)` — largest integer ≤ x (wasm `f64.floor`).
    Floor,
    /// `Math.ceil(x)` — smallest integer ≥ x (wasm `f64.ceil`).
    Ceil,
    /// `Math.round(x)` — JS rounding `floor(x + 0.5)` (so `round(2.5) == 3` and
    /// `round(-2.5) == -2`), which differs from wasm's banker's `f64.nearest`.
    Round,
    /// `Math.trunc(x)` — integer part toward zero (wasm `f64.trunc`).
    Trunc,
    /// `Math.abs(x)` — magnitude (wasm `f64.abs`).
    Abs,
    /// `Math.sqrt(x)` — square root (wasm `f64.sqrt`).
    Sqrt,
    /// `Math.min(..)` — variadic minimum; empty args yield `+Infinity`. Folds
    /// wasm `f64.min`, which propagates NaN and orders `-0 < +0` like JS.
    Min,
    /// `Math.max(..)` — variadic maximum; empty args yield `-Infinity`. Folds
    /// wasm `f64.max`, which propagates NaN and orders `+0 > -0` like JS.
    Max,
    /// `Math.pow(base, exp)` — base raised to exp. wasm has no `f64` pow, so it is
    /// emitted inline: a multiply loop over `trunc(|exp|)` (exact for integer
    /// exponents), then the fractional part applied bit by bit via successive
    /// `f64.sqrt` roots of `base`. A negative exponent reciprocates the result; a
    /// negative base with a fractional exponent yields `NaN`.
    Pow,
}

/// An array higher-order method: `map`/`filter`/`reduce`/`forEach`.
/// Recognized syntactically from a `<expr>.<name>(callback[, init])` member callee
/// at codegen (kept distinct from `x[i]` element indexing), each invoking the
/// callback function value per element via the `call_indirect` ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayHofOp {
    /// `arr.map(fn)` — a NEW array of the same length where `result[i] =
    /// fn(element_i, i)`.
    Map,
    /// `arr.filter(fn)` — a NEW array of the elements for which `fn(element_i, i)`
    /// is JS-truthy, grown via the `__array_push` helper.
    Filter,
    /// `arr.reduce(fn, init)` — folds `acc = fn(acc, element_i, i)` from `acc =
    /// init`, yielding the final accumulator. An `init` argument is required.
    Reduce,
    /// `arr.forEach(fn)` — calls `fn(element_i, i)` for side effects and yields
    /// `undefined`.
    ForEach,
}

/// A conversion built-in: `String`/`Number`/`Boolean`/`parseInt`/
/// `parseFloat`. Recognized syntactically from a bare-identifier callee at codegen
/// (only when nothing of the same name shadows it), each taking one argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertOp {
    /// `String(x)` — the JS string form (a heap STRING jsval); reuses the run
    /// stage's `format_value`, so `String(42) == "42"` like `format_number`.
    ToString,
    /// `Number(x)` — the JS numeric coercion (a number jsval); string operands are
    /// full-string parsed (trimmed; empty => 0; non-numeric => NaN).
    ToNumber,
    /// `Boolean(x)` — JS truthiness (a boolean jsval). Lowered to inline wasm.
    ToBoolean,
    /// `parseInt(s)` — leading base-10 integer of the (stringified) argument, or
    /// NaN when none. The optional radix argument is ignored (base-10 only).
    ParseInt,
    /// `parseFloat(s)` — leading decimal float of the (stringified) argument, or
    /// NaN when none.
    ParseFloat,
}

/// A non-callback array/string method. Recognized syntactically from a
/// `<expr>.<name>(args)` member callee at codegen (kept distinct from `x[i]`
/// element indexing), each dispatched host-side through the `__method` import.
/// `IndexOf` and `Slice` exist on BOTH arrays and strings, so the host branches
/// on the receiver's runtime TYPE BYTE (array-element vs string-byte semantics);
/// the rest are single-type methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodOp {
    /// `arr.indexOf(x)` — first index where element `=== x`, else `-1` (a number);
    /// `str.indexOf(sub)` — byte index of the first `sub` occurrence, else `-1`
    /// (empty `sub` => `0`). Overloaded: the receiver's TYPE BYTE selects the arm.
    IndexOf,
    /// `arr.includes(x)` — `indexOf(x) !== -1` as a boolean (array only).
    Includes,
    /// `arr.join(sep)` — concatenate `String(element)` for each element separated
    /// by the `sep` string (`","` when `sep` is omitted); `null`/`undefined`
    /// elements stringify to `""`. Yields a heap string (array only).
    Join,
    /// `arr.slice(a, b)` — a NEW array of elements `[a, b)`; `str.slice(a, b)` — a
    /// NEW substring `[a, b)`. JS negative-from-end and clamp semantics; `b`
    /// omitted => to end. Overloaded on the receiver's TYPE BYTE.
    Slice,
    /// `str.toUpperCase()` — a NEW ASCII-upper heap string (string only).
    ToUpper,
    /// `str.toLowerCase()` — a NEW ASCII-lower heap string (string only).
    ToLower,
    /// `str.charAt(i)` — a NEW one-char heap string at byte index `i` (`""` when
    /// out of range) (string only).
    CharAt,
    /// `str.trim()` — a NEW heap string with leading and trailing ASCII whitespace
    /// removed (string only).
    Trim,
    /// `str.trimStart()` — a NEW heap string with leading ASCII whitespace removed
    /// (string only).
    TrimStart,
    /// `str.trimEnd()` — a NEW heap string with trailing ASCII whitespace removed
    /// (string only).
    TrimEnd,
    /// `str.repeat(n)` — a NEW heap string of the receiver repeated `n` times
    /// (`n` truncated to an integer; a negative or non-finite `n` yields `""` —
    /// JS throws a `RangeError`, a documented limitation) (string only).
    Repeat,
    /// `str.startsWith(sub)` — `true` when the receiver begins with `sub` (a
    /// boolean; `sub` is coerced to its string form) (string only).
    StartsWith,
    /// `str.endsWith(sub)` — `true` when the receiver ends with `sub` (a boolean;
    /// `sub` is coerced to its string form) (string only).
    EndsWith,
    /// `str.padStart(len, pad)` — a NEW heap string left-padded to byte length
    /// `len` with `pad` (default `" "`); the receiver is returned unchanged when it
    /// is already at least `len` bytes or `pad` is empty (string only).
    PadStart,
    /// `str.padEnd(len, pad)` — a NEW heap string right-padded to byte length `len`
    /// with `pad` (default `" "`); the receiver is returned unchanged when it is
    /// already at least `len` bytes or `pad` is empty (string only).
    PadEnd,
    /// `str.substring(a, b)` — a NEW substring `[a, b)` by BYTE index (each endpoint
    /// truncated to an integer, negatives/NaN clamped to `0`, and the two swapped
    /// when `a > b`; `b` omitted => to end) (string only).
    Substring,
    /// `str.split(sep)` — a NEW array of substrings split on each `sep` occurrence
    /// (`sep` omitted => `[str]`; `sep === ""` => one element per byte; BYTE
    /// semantics, `sep` coerced to its string form) (string only).
    Split,
}

/// An `Object.*` collection built-in: `Object.keys`/`values`/`entries`.
/// `Object` is not a real binding — these are recognized syntactically from an
/// `Object.<name>(arg)` member callee at codegen (only when nothing named
/// `Object` shadows it) and each takes exactly one object argument, returning a
/// fresh growable array built in-wasm from the object's runtime key table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectBuiltinOp {
    /// `Object.keys(o)` — a NEW array of the object's own enumerable key strings,
    /// integer keys ascending first, then string keys in insertion order.
    Keys,
    /// `Object.values(o)` — a NEW array of the object's own enumerable values, in
    /// [`Self::Keys`] order.
    Values,
    /// `Object.entries(o)` — a NEW array of `[key, value]` two-element arrays, one
    /// per own enumerable property in [`Self::Keys`] order.
    Entries,
    /// The `for-in` key snapshot (not a JS builtin): a NEW array of the
    /// enumerable string keys of `o` and its prototype chain, each listed once
    /// (an own key shadows an inherited one), taken at loop entry.
    ForInKeys,
}

/// The key of one [`IrNode::Object`] entry.
#[derive(Debug, Clone, PartialEq)]
pub enum PropKey {
    /// A compile-time string key of an enumerable data property.
    Static(String),
    /// A compile-time string key of a NON-enumerable (writable, configurable)
    /// property: class prototype methods/accessors, `constructor`, static
    /// methods, and an error's own `message` / `stack`.
    Hidden(String),
    /// A compile-time string key of a non-writable, non-enumerable,
    /// non-configurable data property: a class's `prototype`.
    Fixed(String),
    /// A computed key (`{[k]: v}`, or a numeric literal key `{1: v}`), converted
    /// with `ToPropertyKey` and interned at run time.
    Computed(IrNode),
}

/// What the right-hand side of an `instanceof` is (design 4.6). Any expression
/// is accepted: the general case is [`InstanceOfKind::Ctor`]
/// (`OrdinaryHasInstance`). The builtin names that are not yet runtime values
/// are recognized syntactically at codegen when no binding shadows them: the
/// three builtin error constructors walk the receiver's `proto` chain for their
/// static prototype ([`InstanceOfKind::Proto`]).
///
/// The builtin constructors `Array`/`Object`/`Function`/`Date` match by the
/// receiver's runtime TYPE BYTE rather than a prototype hop: `Array` /
/// `Function` / `Date` match their own type byte, and `Object` matches ANY heap
/// pointer that is not a primitive string (so arrays, plain objects, functions,
/// maps, sets, promises, generators, errors, and dates are all `instanceof
/// Object`, while a string/number/boolean/null/undefined is not).
#[derive(Debug, Clone, PartialEq)]
pub enum InstanceOfKind {
    /// `instanceof Array` — the receiver is a heap `TYPE_ARRAY` block.
    Array,
    /// `instanceof Function` — the receiver is a heap `TYPE_FUNCTION` value.
    Function,
    /// `instanceof Date` — the receiver is a heap `TYPE_DATE` block.
    Date,
    /// `instanceof Object` — the receiver is any heap pointer that is not a
    /// primitive string.
    Object,
    /// `instanceof E` for an unshadowed builtin error constructor: some object
    /// on the receiver's `proto` chain must be the SAME object as the static
    /// prototype the carried [`IrNode::ErrorProto`] yields. The walk starts at
    /// the receiver's `proto` slot and follows each prototype's own `proto`
    /// until a match or `null`.
    Proto(Box<IrNode>),
    /// `instanceof C` for any other right-hand side expression:
    /// `OrdinaryHasInstance(C, x)`. A non-callable `C` throws a `TypeError`; a
    /// non-object receiver is `false`; otherwise `Get(C, "prototype")` (a
    /// non-object one throws a `TypeError`) is searched for on the receiver's
    /// `proto` chain like [`InstanceOfKind::Proto`].
    Ctor(Box<IrNode>),
}

/// A builtin error kind with a static prototype object in the data segment
/// (`%Error.prototype%`, `%TypeError.prototype%`, `%RangeError.prototype%`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `Error`.
    Error,
    /// `TypeError` (its prototype inherits from `%Error.prototype%`).
    TypeError,
    /// `RangeError` (its prototype inherits from `%Error.prototype%`).
    RangeError,
}

impl ErrorKind {
    /// Every kind, in static-prototype layout order.
    pub(crate) const ALL: [ErrorKind; 3] = [Self::Error, Self::TypeError, Self::RangeError];

    /// The kind named by the constructor identifier `name`.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The constructor name, which is also the prototype's `name` value.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::TypeError => "TypeError",
            Self::RangeError => "RangeError",
        }
    }
}

/// The `TypeError` a call or `new` site throws when its callee is not callable
/// (or not a constructor), named by codegen after the callee's source text:
/// `x` for an identifier, `o.m` for a member with a static key, `expression`
/// otherwise. Carries the error's `.message` and its single-line `.stack`;
/// both are static strings interned wherever the site can throw.
#[derive(Debug, Clone, PartialEq)]
pub struct CalleeError {
    /// The error's `.message`, e.g. `x is not a function`.
    pub message: String,
    /// The error's `.stack` first line, `TypeError: <message>`.
    pub stack: String,
}

impl CalleeError {
    /// The source name of a callee with no nameable source text.
    pub(crate) const EXPRESSION: &'static str = "expression";

    /// `<name> is not a function`.
    pub(crate) fn not_a_function(name: &str) -> Self {
        Self::with_message(format!("{name} is not a function"))
    }

    /// `<name> is not a constructor`.
    pub(crate) fn not_a_constructor(name: &str) -> Self {
        Self::with_message(format!("{name} is not a constructor"))
    }

    fn with_message(message: String) -> Self {
        let stack = format!("TypeError: {message}");
        Self { message, stack }
    }
}

/// A `Map`/`Set` instance method. Recognized syntactically from a
/// `<expr>.<name>(args)` member callee at codegen; because these names
/// (`set`/`get`/`has`/`add`/`delete`) can ALSO be ordinary object/class methods
/// (e.g. `class C { add(x) {..} }`), the lowered [`IrNode::MapMethod`] dispatches
/// at RUNTIME on the receiver's TYPE BYTE: a `TYPE_MAP`/`TYPE_SET` handle takes the
/// collection op, anything else falls back to a plain user method call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapOp {
    /// `map.set(k, v)` — insert or update the entry for key `k`, yielding the map
    /// (chainable). Linear `===` scan; a new key appends `(k, v)` to the backing.
    Set,
    /// `map.get(k)` — the value stored under `k` (`===` scan), or `undefined`.
    Get,
    /// `map.has(k)` / `set.has(v)` — `true` iff a `===`-equal key/value is present.
    Has,
    /// `set.add(v)` — insert `v` (with an `undefined` value slot) if not already
    /// present (`===` dedup), yielding the set (chainable).
    Add,
    /// `map.delete(k)` / `set.delete(v)` — remove the matching entry, yielding
    /// `true` iff one was present.
    Delete,
}

/// A `Date` instance method. Like [`MapOp`], these names can ALSO be
/// ordinary user methods, so the lowered [`IrNode::DateMethod`] dispatches at
/// RUNTIME on the receiver's TYPE BYTE: a `TYPE_DATE` block computes the field
/// GUEST-side from its stored f64 epoch-ms, and anything else falls back to a
/// plain user method call. All calendar/time fields are **UTC**, derived from the
/// stored milliseconds with the standard civil-from-days algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOp {
    /// `d.valueOf()` — the stored millisecond timestamp (identical to
    /// `d.getTime()`), as a number.
    ValueOf,
    /// `d.getFullYear()` — the UTC calendar year (e.g. `1970`).
    GetFullYear,
    /// `d.getMonth()` — the UTC month, **0-based** (`0` = January).
    GetMonth,
    /// `d.getDate()` — the UTC day of the month, 1-based (`1`..`31`).
    GetDate,
    /// `d.getDay()` — the UTC day of the week, `0` = Sunday .. `6` = Saturday.
    GetDay,
    /// `d.getHours()` — the UTC hour of the day (`0`..`23`).
    GetHours,
    /// `d.getMinutes()` — the UTC minute of the hour (`0`..`59`).
    GetMinutes,
    /// `d.getSeconds()` — the UTC second of the minute (`0`..`59`).
    GetSeconds,
    /// `d.toISOString()` — the ISO 8601 UTC string `YYYY-MM-DDTHH:MM:SS.mmmZ`,
    /// built GUEST-side as a fresh heap string.
    ToIsoString,
}

/// A promise / generator instance method claimed from a `<expr>.<name>(args)`
/// member callee at codegen. These names (`then`/`catch`/`finally`/`next`) are
/// just as common as ordinary user methods (thenables, iterator objects), so the
/// lowered [`IrNode::ReceiverMethod`] dispatches at RUNTIME on the receiver's TYPE
/// BYTE: a promise (resp. generator) takes the builtin path, anything else falls
/// back to a plain user method call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverOp {
    /// `p.then(onF, onR)` on a `TYPE_PROMISE` receiver.
    Then,
    /// `p.catch(onR)` on a `TYPE_PROMISE` receiver.
    Catch,
    /// `p.finally(fn)` on a `TYPE_PROMISE` receiver.
    Finally,
    /// `gen.next(v)` on a `TYPE_GENERATOR` receiver.
    Next,
}

impl ReceiverOp {
    /// The source method name, used as the member key of the user-method fallback.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Then => "then",
            Self::Catch => "catch",
            Self::Finally => "finally",
            Self::Next => "next",
        }
    }
}

/// One element of a spread-capable sequence — an array literal or a call's
/// argument list — that contains at least one `...` source. The whole sequence
/// is materialized at runtime (its length is not known until then), appending
/// each part left-to-right.
#[derive(Debug, Clone, PartialEq)]
pub enum SpreadElem {
    /// A plain element/argument `e`, appended as a single value.
    Item(IrNode),
    /// A spread source `...e`: every element of the (array) value is appended in
    /// order.
    Spread(IrNode),
}

/// How a spread call reaches its callee, mirroring the direct/indirect
/// split of [`IrNode::Call`] / [`IrNode::CallIndirect`].
#[derive(Debug, Clone, PartialEq)]
pub enum SpreadCallee {
    /// A direct call to a known function declaration — its codegen function
    /// index (the same value carried by [`IrNode::Call`]'s `func`).
    Direct(u32),
    /// An indirect call through a computed function value, with the
    /// `TypeError` it throws when that value is not callable.
    Indirect(Box<IrNode>, CalleeError),
    /// A `super(...args)` call: the parent constructor value, invoked with
    /// `this` kept as the enclosing constructor's snapshot in `this_local`
    /// (like [`IrNode::ThisCall`]) and the construct marker as `argc`.
    This {
        /// The parent constructor function value.
        callee: Box<IrNode>,
        /// The enclosing constructor's `this` snapshot local.
        this_local: u32,
    },
    /// A method call `obj.m(...args)` / `obj[k](...args)`: `callee` stores the
    /// receiver into `this_local` and then reads the method from it, so the
    /// call binds `this` = the receiver (like [`IrNode::MethodCallUser`]).
    Method {
        /// The receiver store followed by the member read.
        callee: Box<IrNode>,
        /// The local holding the receiver, bound as `this` for the call.
        this_local: u32,
        /// The `TypeError` thrown when the member is not callable.
        not_callable: CalleeError,
    },
}

/// A node in the numeric-expression IR tree.
#[derive(Debug, Clone, PartialEq)]
pub enum IrNode {
    /// A numeric literal, stored as an `f64`.
    Num(f64),
    /// A string literal. It lowers to a heap-pointer jsval (single heap tag
    /// `0b101`) whose payload is the compile-time byte offset of the literal's
    /// block base in the emitted linear memory's static data. The block is
    /// `[type=0][pad×3][len:u32][utf8]` — a `HEADER_SIZE`-byte type header
    /// in front of the `[len][utf8]` payload.
    Str(String),
    /// A boolean literal (`true`/`false`).
    Bool(bool),
    /// The `null` literal.
    Null,
    /// The `undefined` value (the global identifier `undefined`).
    Undefined,
    /// An elision in an array literal (`[1, , 3]`): the raw [`crate::abi::HOLE`]
    /// element bits. Only ever an element of [`IrNode::Array`] or an item of
    /// [`IrNode::ArraySpread`], so it never reaches JS as a value.
    ArrayHole,
    /// A binary operation applied to two sub-expressions.
    Binary {
        /// The operator to apply.
        op: BinOp,
        /// The left-hand operand.
        lhs: Box<IrNode>,
        /// The right-hand operand.
        rhs: Box<IrNode>,
    },
    /// Read a local variable by its wasm local index (`local.get`).
    Local(u32),
    /// Assign `value` to a local and yield the assigned value (`local.tee`).
    Assign {
        /// The wasm local index to write.
        local: u32,
        /// The expression whose value is stored and produced.
        value: Box<IrNode>,
    },
    /// Call a statically-known user-defined function by its wasm function index
    /// (a direct `call`, kept as an optimization for calls to a function
    /// *declaration* by name). Under the uniform ABI the callee's wasm
    /// signature is `(a0..a_{K-1}, env) -> i64`, so the backend pushes the
    /// `args` — padded with `undefined` (or truncated) to the fixed `K`
    /// positional slots — then a trailing `env` (`undefined` for a direct call,
    /// since a top-level declaration never captures).
    Call {
        /// The wasm function index of the callee.
        func: u32,
        /// The argument expressions, in source order.
        args: Vec<IrNode>,
    },
    /// A first-class **function value** built from a function declaration named
    /// in value position (`let f = add;`, `return add;`, passing `add` as an
    /// argument), from an arrow, or from a function expression. It lowers to a
    /// heap block (type byte `3`) holding the function's
    /// **funcref-table index** and an environment slot. For a non-capturing
    /// function `captures` is empty and the env slot is `undefined`; for a
    /// **closure** `captures` lists the source expressions — one per env
    /// slot, in slot order — that each yield a boxed-cell pointer, and the env
    /// slot points at a freshly built environment block holding those cells.
    FuncValue {
        /// The index of the function in the module's funcref table (the wasm
        /// function index minus one, since function index `0` is `main`).
        table_index: u32,
        /// The captured-cell source expressions, one per environment slot in
        /// slot order (empty for a non-capturing function). Each yields the
        /// jsval pointer of the boxed cell to store in the environment.
        captures: Vec<IrNode>,
        /// The function's constructor kind (its [`FnInfo::kind`]), selecting the
        /// header `flags` the object is created with.
        kind: FnKind,
    },
    /// The static prototype object of a builtin error kind (a heap-pointer
    /// jsval to a data-segment block, a compile-time constant).
    ErrorProto(ErrorKind),
    /// The function object of a top-level function declaration (funcref-table
    /// index `.0`). A top-level declaration never captures, so its one object
    /// is a static block in the data segment: every reference yields the same
    /// identity, and properties added to it persist.
    DeclFunc(u32),
    /// Allocate a **boxed cell** (heap block, type byte `5`) holding the value of
    /// the sub-expression, and yield the cell's heap-pointer jsval. A
    /// captured `let`/`const`/parameter binding is stored through such a cell so
    /// that a closure capturing it shares its mutations by reference.
    CellNew(Box<IrNode>),
    /// Read the value stored in a boxed cell: evaluate the sub-expression to a
    /// cell pointer, then load the `jsval` at `cell + HEADER_SIZE`.
    CellGet(Box<IrNode>),
    /// Store `value` into the boxed cell named by `cell` and yield the stored
    /// value. Both sub-expressions are cell/local reads with no side
    /// effects, so evaluating `cell` twice (as some desugarings do) is safe.
    CellSet {
        /// The expression yielding the target cell pointer.
        cell: Box<IrNode>,
        /// The value to store (also the expression's result).
        value: Box<IrNode>,
    },
    /// Load the boxed-cell pointer at environment slot `slot` of the currently
    /// executing closure: the environment jsval is the last wasm
    /// parameter (the uniform ABI's `env` slot); this reads the cell pointer at
    /// `env + HEADER_SIZE + 4 + 8*slot`. Used inside a closure body to reach a
    /// captured binding's cell.
    EnvLoad(u32),
    /// Load the boxed-cell pointer of **module cell** `slot`: a top-level binding
    /// referenced by a top-level function lives in a cell whose pointer is held in
    /// a dedicated wasm global, so every function — top-level declaration, arrow,
    /// method — reaches it without a closure env. `main`'s prologue allocates each
    /// cell holding the TDZ hole (see [`IrNode::IsHole`]) before any user code.
    ModuleCell(u32),
    /// Whether the sub-expression's value is the TDZ hole a module cell holds until
    /// its binding's declaration runs, as a boolean jsval. Guards accesses to a
    /// module binding from a function body, which may run before the declaration.
    IsHole(Box<IrNode>),
    /// The TDZ hole itself: a derived constructor's `this` before `super(..)`.
    Hole,
    /// Whether the value is an object (a heap value other than a string), as a
    /// boolean jsval.
    IsObject(Box<IrNode>),
    /// A class heritage `extends value` (design 4.5): evaluate `value` into
    /// `parent`; unless it is `null` it must be a constructor, and `proto` gets
    /// `Get(parent, "prototype")`, which must be an object or `null` (`proto` is
    /// `null` for a `null` parent); either failure throws a `TypeError` naming
    /// the value. Yields `undefined`.
    ClassHeritage {
        /// The `extends` expression.
        value: Box<IrNode>,
        /// Local receiving the parent constructor (or `null`).
        parent: u32,
        /// Local receiving the parent's `prototype` (or `null`).
        proto: u32,
    },
    /// Store `proto` into the `proto` slot of the object-family value `obj` and
    /// yield `obj`. `obj` is read twice, so it must be side-effect free.
    SetProto {
        /// The object whose prototype is set.
        obj: Box<IrNode>,
        /// The new prototype (an object or `null`).
        proto: Box<IrNode>,
    },
    /// A `yield expr` expression. Produced by lowering a generator
    /// body; the generator segmentation pass CONSUMES every `Yield`, turning it
    /// into a state-machine suspend point, so a `Yield` that survives to the
    /// backend is an unsupported yield position (rejected by codegen before emit).
    Yield(Box<IrNode>),
    /// Read the value passed to the generator's `.next(v)` for the `yield` that is
    /// resuming: the state machine's first wasm parameter (local `0`,
    /// `sent`). Emitted at the start of a resumed segment to bind `x = yield …`.
    SentValue,
    /// Build a `Generator` handle: allocate the [`abi::TYPE_GENERATOR`]
    /// block, allocate `total` boxed cells for the frame (the first `slots.len()`
    /// initialized from the constructor's argument expressions, the rest to
    /// `undefined`), stash them in a `TYPE_ENV` frame block stored in the handle's
    /// `env` field, set `sm_idx`/`state=0`/`done=0`, and yield the handle jsval.
    /// This is the whole body of a `function*` CONSTRUCTOR.
    MakeGenerator {
        /// The funcref-table index of the state-machine function.
        sm_table_index: u32,
        /// Initial values for the leading frame cells (the generator parameters),
        /// in slot order — each a `Local(i)` reading the constructor's argument.
        slots: Vec<IrNode>,
        /// The total number of frame cells (parameters plus every body local).
        total: u32,
    },
    /// Whether the sub-expression is a generator handle: yields a
    /// boolean jsval. Used by `for-of`/spread to branch between driving a
    /// generator via `.next()` and the ordinary array/string index walk.
    IsGenerator(Box<IrNode>),
    /// Drive a generator one step — `gen.next(sent)`. Evaluate `gen` to
    /// a handle, `call_indirect` its state machine with `(sent, gen)`, then build
    /// the `{ value, done }` result object from the returned value and the
    /// handle's `done` flag. A non-generator receiver throws a `TypeError`.
    GenResume {
        /// The expression producing the generator handle.
        handle: Box<IrNode>,
        /// The value passed as the resumed `yield`'s result (`undefined` for a
        /// bare `.next()`).
        sent: Box<IrNode>,
    },
    /// Call a function **value** — an expression yielding a function heap block
    /// (a variable, parameter, or returned value) whose callee cannot be
    /// resolved statically. It lowers to a `call_indirect` against the funcref
    /// table using the table index read from the block, under the same uniform
    /// ABI as [`IrNode::Call`]. Missing arguments become `undefined`; extra
    /// arguments are ignored.
    CallIndirect {
        /// The expression producing the callee function value.
        callee: Box<IrNode>,
        /// The argument expressions, in source order.
        args: Vec<IrNode>,
        /// The `TypeError` thrown when the callee is not callable, named after
        /// the callee's source text.
        not_callable: CalleeError,
    },
    /// The unary `typeof` operator applied to a sub-expression. It yields a
    /// boxed type-string immediate (tag `0b100`) naming the operand's runtime
    /// type (`"number"`/`"boolean"`/`"object"`/`"undefined"`).
    TypeOf(Box<IrNode>),
    /// An array literal `[e0, e1, ..]`. It lowers to a heap array (single heap tag
    /// `0b101`, type byte `1`): a `cap = n` `TYPE_ELEMS` store holding each
    /// element jsval at `store + 8 + 8*k`, then the 24-byte array handle (object
    /// prefix, `len = n`, `elems = store`).
    /// Elements are evaluated left-to-right; an elision is an
    /// [`IrNode::ArrayHole`] element.
    Array(Vec<IrNode>),
    /// An array literal containing at least one spread element `[0, ...a, 9]`.
    /// Built at runtime: start from a fresh empty array and, left-to-
    /// right, append each plain element (via `__array_push`) or every element of
    /// each spread source (via `__array_concat`). Yields the finished array's
    /// heap jsval.
    ArraySpread(Vec<SpreadElem>),
    /// A call whose argument list contains a spread `f(...a)` / `add(1, ...xs)`.
    /// The arguments are first materialized into a heap array (the same
    /// build as [`IrNode::ArraySpread`], carried by `args`); the call then
    /// spreads that array's elements into the uniform ABI's fixed `K` positional
    /// slots (padding with `undefined`) and passes the array's length as the
    /// dynamic `argc`; the array itself is published in `__spread_args` for a
    /// callee gathering the arguments beyond `K` ([`IrNode::SpreadArgsGlobal`]).
    CallSpread {
        /// How to reach the callee (direct index or computed value).
        target: SpreadCallee,
        /// The argument array to spread — an [`IrNode::ArraySpread`].
        args: Box<IrNode>,
    },
    /// `Math.max(...)` / `Math.min(...)` with a spread argument. Builds
    /// the argument array (like [`IrNode::ArraySpread`], carried by `args`) then
    /// folds `f64.max` / `f64.min` over its elements from the identity
    /// (`-Infinity` / `+Infinity`), yielding a number jsval.
    MathSpread {
        /// The fold operation — always [`MathOp::Max`] or [`MathOp::Min`].
        op: MathOp,
        /// The argument array to fold — an [`IrNode::ArraySpread`].
        args: Box<IrNode>,
    },
    /// An object literal `{k0: e0, [k1]: e1, ..}`: a `TYPE_OBJECT` block whose
    /// `TYPE_PROPS` table is sized `max(entries.len(), reserve)` up front. Each
    /// entry's key and value are evaluated left-to-right and defined as an own
    /// property (a later duplicate key overwrites the earlier slot in place). An
    /// [`IrNode::MakeAccessor`] value defines an accessor property. `proto` is
    /// the `proto` slot jsval (`null` when absent), `class` its header `class`
    /// byte.
    Object {
        /// The entries, in source order.
        entries: Vec<(PropKey, IrNode)>,
        /// The prototype link (a class instance's or derived prototype's
        /// parent, an error's kind prototype).
        proto: Option<Box<IrNode>>,
        /// The `class` byte ([`crate::abi::CLASS_ORDINARY`],
        /// [`crate::abi::CLASS_ERROR`] or [`crate::abi::CLASS_ARGUMENTS`]).
        class: u8,
        /// Extra table capacity to reserve beyond the entries (a class
        /// instance's field-scan count).
        reserve: u32,
    },
    /// Define own properties on an existing object: `obj` is evaluated once
    /// into `local`, then each entry is defined (`__define`) in order with the
    /// attributes its [`PropKey`] selects; yields `obj`. Entry values may read
    /// `local` (a class prototype's `constructor` back-link to the class).
    DefineProps {
        /// The object to define the properties on.
        obj: Box<IrNode>,
        /// Fresh temp local holding the evaluated `obj`.
        local: u32,
        /// The entries, in definition order.
        entries: Vec<(PropKey, IrNode)>,
    },
    /// An object rest binding `let {a, ...rest} = obj`. Builds a NEW heap
    /// object holding every own key of `source` EXCEPT the statically-known
    /// destructured keys in `exclude`. `source` is evaluated once (a hidden temp
    /// read); `exclude` is an [`IrNode::Array`] of [`IrNode::Str`] key names. It
    /// lowers to the `__rest` runtime helper, which defines every own enumerable
    /// property of the source whose key is not excluded on a fresh object.
    ObjectRest {
        /// The source object being spread (a heap object jsval, evaluated once).
        source: Box<IrNode>,
        /// The excluded keys — an [`IrNode::Array`] of [`IrNode::Str`] names.
        exclude: Box<IrNode>,
    },
    /// The `.length` member, dispatched on the receiver kind (DF2): a string or
    /// array reads its u32 length (a string's at `+ HEADER_SIZE`, an array
    /// handle's at `+ ARRAY_LEN_OFFSET`) as a number jsval; any other heap value reads its `length` property through
    /// `__get`; a primitive yields `undefined`. Codegen wraps the receiver in a
    /// [`IrNode::NullishCheck`].
    StrLen(Box<IrNode>),
    /// The nullish-receiver guard of a builtin member access (`recv.length`,
    /// `recv.push(..)`, `recv.get(..)`, ...): yields `value` unchanged, but throws
    /// the `TypeError` `Cannot read properties of <null|undefined>
    /// (reading '<key>')` when it is `null`/`undefined`. Wrapped around the
    /// receiver by codegen; elided at emit for a statically non-nullish value.
    NullishCheck {
        /// The receiver expression being guarded.
        value: Box<IrNode>,
        /// The property name the access reads (for the error message).
        key: String,
    },
    /// A member read `obj[index]` (static reads `obj.a` lower here as
    /// `obj["a"]`). A string/array with a numeric (or `"length"`) key reads its
    /// character/element/length; every other read is the generic `__get` with
    /// the key converted by `ToPropertyKey` (a static string key is a constant).
    Index {
        /// The receiver.
        obj: Box<IrNode>,
        /// The property key.
        index: Box<IrNode>,
    },
    /// An **accessor** property value (class or object-literal getters/setters):
    /// build a `TYPE_ACCESSOR` heap block holding the getter and setter FUNCTION
    /// VALUES (`undefined` for an absent half). As an [`IrNode::Object`] /
    /// [`IrNode::DefineProps`] entry value it defines an `ATTR_ACCESSOR`
    /// property, so a member READ that resolves to it invokes `getter()` with
    /// `this` = the receiver, and a member WRITE invokes `setter(value)`. Both
    /// halves are ordinary lifted function values that read `this` through the
    /// `this` carrier, exactly like a method.
    MakeAccessor {
        /// The getter function value, or [`IrNode::Undefined`] if the property
        /// declares only a setter.
        getter: Box<IrNode>,
        /// The setter function value, or [`IrNode::Undefined`] if the property
        /// declares only a getter.
        setter: Box<IrNode>,
    },
    /// Member assignment `obj[index] = value`, yielding `value`. An array stores
    /// an in-range element (no growth yet); every other receiver goes through
    /// the generic `__set` (adding the property when absent).
    IndexSet {
        /// The receiver.
        obj: Box<IrNode>,
        /// The property key.
        index: Box<IrNode>,
        /// The jsval stored into the slot; also the expression's result.
        value: Box<IrNode>,
    },
    /// Compound element assignment `obj[index] op= rhs` (`+=`/`-=`/`*=`/`/=`/`%=`)
    /// on an array element or object member. `obj` and `index` are each
    /// evaluated EXACTLY ONCE into the fresh temp locals `obj_local`/`idx_local`,
    /// the current member value is read back through those temps, combined with
    /// `rhs` via `op` (reusing the [`IrNode::Binary`] overload — so `+=` on a
    /// string member concatenates), and the result is stored back into the same
    /// member and yielded. The runtime type-byte dispatch mirrors
    /// [`IrNode::IndexSet`]. The new value is staged in `new_local` so the final
    /// store reads its operands from stable temps rather than clobbered scratch.
    IndexOpSet {
        /// The array/object being updated (evaluated once into `obj_local`).
        obj: Box<IrNode>,
        /// The index/key (evaluated once into `idx_local`).
        index: Box<IrNode>,
        /// The arithmetic operator the compound assignment applies.
        op: BinOp,
        /// The right-hand operand combined with the current member value.
        rhs: Box<IrNode>,
        /// Fresh temp local holding the evaluated `obj` pointer.
        obj_local: u32,
        /// Fresh temp local holding the evaluated `index` jsval.
        idx_local: u32,
        /// Fresh temp local holding the computed new member value.
        new_local: u32,
        /// Whether this is a plain compound assignment or a member `++`/`--`.
        kind: OpSetKind,
    },
    /// `array.push(value)` on the array handle held in a wasm local. Appends
    /// `value` to the handle in place (`__array_push`, amortized store growth;
    /// the handle never moves, so every alias observes it). Yields the new
    /// length as a number jsval (JS semantics). Codegen evaluates any receiver
    /// expression into the local and guards it against `null`/`undefined`
    /// before this node runs.
    ArrayPush {
        /// The wasm local holding the array handle.
        local: u32,
        /// The value appended at the new last slot.
        value: Box<IrNode>,
        /// The `TypeError` (`<recv>.push is not a function`) thrown, after
        /// `value` is evaluated, when the local is not a `TYPE_ARRAY` handle;
        /// `None` for a compiler-built receiver that is always an array.
        not_array: Option<CalleeError>,
    },
    /// `array.pop()` on the array handle held in a wasm local (receiver
    /// evaluation and guards as for [`IrNode::ArrayPush`]). Shrinks the handle
    /// in place: its u32 length is decremented and the removed last element
    /// jsval is yielded. An empty array yields `undefined`.
    ArrayPop {
        /// The wasm local holding the array whose block is decremented in place.
        local: u32,
        /// The `TypeError` (`<recv>.pop is not a function`) thrown when the
        /// local is not a `TYPE_ARRAY` handle.
        not_array: CalleeError,
    },
    /// An array higher-order method call `array.map/filter/reduce/forEach(...)`.
    /// The `array` receiver is evaluated once into a scratch base (so any array
    /// expression works),
    /// then a counted wasm loop over its length invokes the `callback` FUNCTION
    /// value per element through the `call_indirect` ABI (env-last): the
    /// positional args are `(element, index)` for `Map`/`Filter`/`ForEach` and
    /// `(acc, element, index)` for `Reduce`, padded with `undefined` to the shared
    /// funcref arity. `Map` fills a fresh same-length array; `Filter` grows a fresh
    /// array via `__array_push` for JS-truthy results; `Reduce` folds into `acc`
    /// (seeded from `init`); `ForEach` discards each result and yields `undefined`.
    ArrayHof {
        /// Which higher-order method this call performs.
        op: ArrayHofOp,
        /// The receiver array expression (evaluated once; not rebound).
        array: Box<IrNode>,
        /// The callback expression, yielding a FUNCTION value.
        callback: Box<IrNode>,
        /// The initial accumulator for [`ArrayHofOp::Reduce`] (`None` for the
        /// others).
        init: Option<Box<IrNode>>,
    },
    /// A non-callback array/string method call `recv.method(args)`. The
    /// receiver is evaluated once, then the whole operation runs host-side through
    /// the `__method` import, which decodes the receiver (reading linear memory)
    /// and branches on its runtime TYPE BYTE for the overloaded `IndexOf`/`Slice`
    /// (array-element vs string-byte semantics), bump-allocating any result string
    /// or array in guest memory. The (already arity-checked) `args` are passed in
    /// two fixed jsval slots, missing trailing ones padded with `undefined`.
    MethodCall {
        /// Which method this call performs.
        op: MethodOp,
        /// The receiver expression (an array or string; evaluated once).
        recv: Box<IrNode>,
        /// The argument expressions in source order (0..=2; see [`MethodOp`]).
        args: Vec<IrNode>,
    },
    /// The unary logical NOT `!operand`. Evaluates `operand` under JS
    /// truthiness and yields the **boolean** jsval that is its inverse
    /// (`!truthy(operand)`). This is the only logical operator that produces a
    /// boolean rather than an operand value.
    Not(Box<IrNode>),
    /// A short-circuit logical operation `lhs op rhs` (`&&`/`||`/`??`). The left
    /// operand is evaluated once into a temp local and tested; the right operand
    /// is evaluated only on the non-short-circuit path. The result is the
    /// selected **operand jsval** (JS value semantics), not a coerced boolean.
    Logical {
        /// The short-circuit operator to apply.
        op: LogicalOp,
        /// The left-hand operand (evaluated once, then tested).
        lhs: Box<IrNode>,
        /// The right-hand operand (evaluated only when not short-circuited).
        rhs: Box<IrNode>,
    },
    /// Increment/decrement a local variable (`++`/`--`), reading and writing the
    /// local exactly once. `inc` selects `+1` (`++`) over `-1` (`--`); the stored
    /// value is always the updated one. `prefix` selects the produced value:
    /// prefix (`++x`/`--x`) yields the NEW value, postfix (`x++`/`x--`) yields the
    /// OLD value. Desugars a compound `x += 1` / `x -= 1` while preserving the
    /// prefix/postfix result distinction.
    IncDec {
        /// The wasm local index to update.
        local: u32,
        /// `true` for `++` (increment), `false` for `--` (decrement).
        inc: bool,
        /// `true` for the prefix form (yields the new value); `false` for postfix
        /// (yields the old value).
        prefix: bool,
    },
    /// A `console.log(a0, a1, ..)` call. It is NOT a user-function call:
    /// it lowers to a sequence of calls to the host `print` import, one per
    /// argument, then yields the `undefined` jsval (its expression value). The
    /// backend formats each argument's jsval on the host side (reusing
    /// `format_value`), separating arguments with a single space and terminating
    /// the line with a newline. `console.log()` with no arguments prints just a
    /// newline. Arguments are evaluated left-to-right.
    ConsoleLog {
        /// The argument expressions, in source order (0..N).
        args: Vec<IrNode>,
    },
    /// A `Math.<name>(a0, a1, ..)` built-in method call. `Math` is not a
    /// real binding: this is recognized syntactically at codegen and lowered to
    /// native wasm `f64` ops (or an inline loop for [`MathOp::Pow`]). Arguments
    /// are evaluated left-to-right; the numeric result is a reboxed number jsval.
    MathCall {
        /// Which `Math.*` method this call invokes.
        op: MathOp,
        /// The argument expressions, in source order (arity is validated at
        /// codegen: one for the unary ops, two for `pow`, any count for
        /// `min`/`max`).
        args: Vec<IrNode>,
    },
    /// A conversion built-in call `String(x)` / `Number(x)` / `Boolean(x)` /
    /// `parseInt(s)` / `parseFloat(s)`. These are bare global function
    /// names, not real bindings: they are recognized syntactically at codegen
    /// (only when nothing of the same name shadows them) and take exactly one
    /// argument (`parseInt` also an optional radix). `Boolean` lowers to inline
    /// JS-truthiness wasm; `parseInt` calls the host `__parse_int` import; the
    /// other three delegate to the host `__convert` import, which reuses the run
    /// stage's `format_value`/parse helpers (so `String(42) == "42"`, matching
    /// `format_number`).
    ConvertCall {
        /// Which conversion this call performs.
        op: ConvertOp,
        /// The first argument expression, evaluated once to a jsval.
        arg: Box<IrNode>,
        /// The `parseInt` radix argument, evaluated after `arg`; `None` for an
        /// omitted radix and for every other conversion.
        radix: Option<Box<IrNode>>,
    },
    /// The conditional (ternary) operator `cond ? then_branch : else_branch`.
    /// `cond` is evaluated under JS truthiness; the selected branch's jsval is
    /// produced (the other branch is not evaluated).
    Ternary {
        /// The condition expression (JS truthiness selects the branch).
        cond: Box<IrNode>,
        /// The expression evaluated when `cond` is truthy.
        then_branch: Box<IrNode>,
        /// The expression evaluated when `cond` is falsy.
        else_branch: Box<IrNode>,
    },
    /// A `throw expr` operation. The sub-expression is evaluated to a
    /// jsval, stashed into the `$exc_value` module global, and the `$exc_pending`
    /// global is set to `1` to mark an exception in flight; control then transfers
    /// to the nearest handler: the enclosing `try` or, absent one, the enclosing
    /// function boundary (an early `return` of the function's dummy result). It
    /// never produces a real value — a `throw` diverges — but as an [`IrNode`] it
    /// nominally leaves an (unreachable) jsval on the stack so it composes in expression position.
    Throw(Box<IrNode>),
    /// The in-flight thrown value: reads the `$exc_value` module global as a jsval.
    /// Only meaningful on a `catch` entry path, where the handler binds it
    /// to the catch parameter (see [`IrStmt::Try`]); the value is written by
    /// [`IrNode::Throw`] and stays valid until the next throw overwrites it.
    ExcValue,
    /// A user method call `obj.m(args)` binding `this = obj`. Distinct from
    /// [`IrNode::CallIndirect`] (which resets `this` to `undefined`) and from the
    /// builtin [`IrNode::MethodCall`] (array/string host methods). `obj` is
    /// evaluated ONCE into the fresh temp `obj_local`; the callee is the function
    /// value at member `key` of `obj` (an ordinary member read through `__get`);
    /// the `__this` global is set to `obj` immediately before the
    /// `call_indirect`, so the callee's prologue snapshots it into its own `this`
    /// slot. Missing arguments become `undefined`; extra arguments are ignored, as
    /// with any indirect call.
    MethodCallUser {
        /// The receiver expression (evaluated once into `obj_local`).
        obj: Box<IrNode>,
        /// The member key naming the method — a heap-string key (`IrNode::Str` for
        /// `obj.m`, or a computed key for `obj[k]`).
        key: Box<IrNode>,
        /// The argument expressions, in source order.
        args: Vec<IrNode>,
        /// Fresh temp local holding the evaluated receiver across argument
        /// evaluation and the `__this` set.
        obj_local: u32,
        /// The `TypeError` thrown when the member is not callable (`o.m is not
        /// a function`).
        not_callable: CalleeError,
    },
    /// Read the `__this` wasm global. Emitted ONLY in a function/arrow
    /// prologue to snapshot the current `this` into the function's local `this`
    /// slot, so a nested method call that re-sets the global does not disturb this
    /// frame's already-captured `this`.
    ThisGlobal,
    /// A `super(..)` / `super.m(..)` call inside a derived class (the E-series
    /// `extends`): invoke the function value `callee` with `this` kept as the
    /// CURRENT instance (the enclosing method's snapshotted `this` in `this_local`)
    /// rather than a fresh receiver. `callee` is the parent class value (its
    /// constructor function) for `super(..)` or a parent-prototype method
    /// (`Super.prototype.m`) for `super.m(..)`. Mirrors [`IrNode::MethodCallUser`]'s
    /// call shape — a not-a-function runtime guard, then the `__this` global is set
    /// to `this_local` immediately before the `call_indirect` — but binds the
    /// existing `this` instead of a member receiver, so the parent constructor's
    /// `this.<field> = ..` mutates the SAME instance.
    ThisCall {
        /// The function value to invoke (the parent constructor or a parent method).
        callee: Box<IrNode>,
        /// The argument expressions, in source order.
        args: Vec<IrNode>,
        /// The enclosing method/constructor's `this` snapshot local, rebound as
        /// `__this` for the call.
        this_local: u32,
        /// `super(..)`: pass the construct marker (`argc = -(n + 1)`), so the
        /// parent class constructor accepts the call.
        construct: bool,
    },
    /// `new callee(args)` (design 4.5): evaluate `callee` into `callee_local` and
    /// the arguments, then `IsConstructor(callee)` (a function with
    /// `FUNC_FLAG_CONSTRUCTOR`, else throw `not_constructor`); allocate `this` = a
    /// fresh ordinary object (in `this_local`) whose `proto` is
    /// `Get(callee, "prototype")` when that is an object, else
    /// `%Object.prototype%`; set `__this = this`, `__new_target = callee`, and call
    /// `callee` with the construct marker `argc = -(n + 1)`. The result is the
    /// return value when it is an object, else `this`.
    Construct {
        /// The constructor expression.
        callee: Box<IrNode>,
        /// The argument expressions, in source order (empty with `spread`).
        args: Vec<IrNode>,
        /// The argument array ([`IrNode::ArraySpread`]) when any argument is a
        /// spread; its length is the dynamic argument count.
        spread: Option<Box<IrNode>>,
        /// Fresh temp local holding the callee across the arguments (and then
        /// the call's return value).
        callee_local: u32,
        /// Fresh temp local holding the allocated `this` across the call.
        this_local: u32,
        /// Property-table capacity pre-reserved on `this` (an optimisation only).
        reserve: u32,
        /// The `TypeError` thrown when `callee` is not a constructor.
        not_constructor: CalleeError,
        /// `super(..)` (`__construct_with`): the `new.target` to construct
        /// with (a side-effect-free read) instead of `callee`.
        new_target: Option<Box<IrNode>>,
    },
    /// Read the `__new_target` wasm global. Emitted ONLY in the prologue of a
    /// function that references `new.target`, guarded by the construct marker.
    NewTargetGlobal,
    /// Whether the `argc` local carries the construct marker `-(n + 1)` (its
    /// number is negative), as a boolean.
    IsConstructCall(u32),
    /// The source argument count `n` held by the `argc` local, decoding the
    /// construct marker `-(n + 1)`, as a number.
    ArgCount(u32),
    /// Read the `__spread_args` wasm global: the argument array of the most
    /// recent spread call or construct, which holds the arguments beyond the
    /// `K` positional slots. Emitted ONLY in the prologue of a function that
    /// gathers its arguments (a rest parameter or `arguments`), which reads it
    /// only when its decoded argument count exceeds `K`.
    SpreadArgsGlobal,
    /// Evaluate each expression left to right, discarding every value but the
    /// last, which is the node's value. Built by codegen (never empty) for
    /// operands evaluated only for their side effects: surplus `Error(..)`
    /// arguments, and the nullish-guarded receiver of a `push`/`pop`.
    Seq(Vec<IrNode>),
    /// An `Object.keys/values/entries(arg)` collection built-in call (or the
    /// `for-in` key snapshot). The `arg` value is evaluated once and handed to
    /// `__own_keys` (`__for_in_keys`), which builds a fresh array in own-key
    /// order (2.6), returned as a heap jsval.
    ObjectBuiltin {
        /// Which `Object.*` collection method this call performs.
        op: ObjectBuiltinOp,
        /// The single object argument, evaluated once to a heap-object jsval.
        arg: Box<IrNode>,
    },
    /// A `key in obj` membership test, yielding a `true`/`false` jsval. Both
    /// operands are evaluated once; the key is converted with `ToPropertyKey` and
    /// `__has` looks it up on `obj` and then its prototype chain.
    ObjectHas {
        /// The property-key operand (a heap-string jsval).
        key: Box<IrNode>,
        /// The object being probed (a heap-object jsval).
        obj: Box<IrNode>,
    },
    /// An `x instanceof RHS` identity test (E-series), yielding a `true`/`false`
    /// jsval. `value` is evaluated once and — via a runtime OBJECT type-byte
    /// guard — a non-heap or non-object operand (number, string, array, `Map`,
    /// …) yields `false` without trapping. For an object, `kind` selects the
    /// test (see [`InstanceOfKind`]).
    InstanceOf {
        /// The left operand (any value).
        value: Box<IrNode>,
        /// What the right-hand side names.
        kind: InstanceOfKind,
    },
    /// A `delete obj.k` property removal, yielding a `true` jsval. Both operands
    /// are evaluated once; `__delete` tombstones the own property (a
    /// non-configurable one throws a `TypeError`), so a later read yields
    /// `undefined`, `in` yields `false`, and every enumerator skips it.
    ObjectDelete {
        /// The object owning the property (a heap-object jsval).
        obj: Box<IrNode>,
        /// The property-key operand (a heap-string jsval).
        key: Box<IrNode>,
    },
    /// A `JSON.stringify(arg)` serialization call, yielding a heap-string
    /// jsval (or `undefined` when `arg` is itself `undefined`/a function). The
    /// `arg` value is evaluated once and serialized entirely in-wasm by a recursive
    /// runtime helper (`__json_str`): numbers/booleans/`null` render literally
    /// (non-finite numbers as `"null"`), strings are quoted + escaped, arrays and
    /// objects recurse (own enumerable keys in own-key order; `undefined`/function values are omitted as object values
    /// and become `"null"` as array elements). Number formatting reuses the
    /// existing `__convert` host import (no NEW host import). See [`IrNode::JsonParse`]
    /// for the inverse.
    JsonStringify {
        /// The single value argument, evaluated once and serialized.
        arg: Box<IrNode>,
    },
    /// A `JSON.parse(text)` call. `arg` is
    /// coerced to a string (via the `__convert` `ToString` import) and handed to
    /// the in-wasm recursive-descent parser `__json_parse`, which materializes a
    /// guest value tree — including OBJECTS with runtime-unknown keys, each
    /// member interned and defined through `__define`. Malformed
    /// input throws an `Error` (`.name` `SyntaxError`). The optional reviver
    /// argument is unsupported and rejected at codegen.
    JsonParse {
        /// The single text argument, evaluated once and parsed.
        arg: Box<IrNode>,
    },
    /// A `new Map()` / `new Set()` construction. Allocates a fresh handle
    /// block (type byte `TYPE_MAP` or `TYPE_SET`) with a zero `size` header and a
    /// freshly bump-allocated empty growable backing array, and yields the handle
    /// heap jsval. Only the empty-constructor form is in scope — `new Map([[k, v]])`
    /// and other constructor arguments are rejected at codegen.
    MapNew {
        /// `true` for `new Set()` (`TYPE_SET`), `false` for `new Map()`
        /// (`TYPE_MAP`). The backing layout is identical; only the type byte (and
        /// thus the render label) differs.
        set: bool,
    },
    /// A `Map`/`Set` method call `recv.<op>(args)`. `recv` is evaluated ONCE
    /// into `recv_local`, then a RUNTIME dispatch on its TYPE BYTE selects the path:
    /// a `TYPE_MAP`/`TYPE_SET` handle runs the collection op (`op`) over the shared
    /// flattened `(key, value)` backing via the `__map_*` runtime helpers; any other
    /// receiver falls back to an ordinary user method call `recv.<method_name>(args)`
    /// (binding `this = recv`), so an object/class method of the same name still
    /// works. The fallback reuses `fallback_obj_local` as its own receiver temp.
    MapMethod {
        /// Which collection op the map/set path performs.
        op: MapOp,
        /// The receiver expression (evaluated once into `recv_local`).
        recv: Box<IrNode>,
        /// The call arguments in source order (`[k]` for get/has/delete/add,
        /// `[k, v]` for set); also threaded verbatim to the user-method fallback.
        args: Vec<IrNode>,
        /// The source method name (`"set"`/`"get"`/…), used as the member key of
        /// the user-method fallback and interned as a static string.
        method_name: String,
        /// Fresh temp local holding the evaluated receiver across the dispatch.
        recv_local: u32,
        /// Fresh temp local for the fallback user method call's own receiver stash.
        fallback_obj_local: u32,
        /// The `TypeError` the user-method fallback throws when the member is
        /// not callable.
        not_callable: CalleeError,
    },
    /// A promise / generator instance method call `recv.<op>(args)`. Like
    /// [`IrNode::MapMethod`], `recv` is evaluated ONCE into `recv_local`, then a
    /// RUNTIME dispatch on its TYPE BYTE selects the path: a `TYPE_PROMISE`
    /// receiver of `then`/`catch`/`finally` runs [`IrNode::PromiseThen`] /
    /// [`IrNode::PromiseFinally`], a `TYPE_GENERATOR` receiver of `next` runs
    /// [`IrNode::GenResume`]; any other receiver falls back to an ordinary user
    /// method call `recv.<op name>(args)` (binding `this = recv`, nullish-guarded),
    /// so a thenable or iterator object of the same shape still works.
    ReceiverMethod {
        /// Which builtin method the promise / generator path performs.
        op: ReceiverOp,
        /// The receiver expression (evaluated once into `recv_local`).
        recv: Box<IrNode>,
        /// The call arguments in source order; the builtin path uses the leading
        /// handler / sent-value slots, the user-method fallback receives them all.
        args: Vec<IrNode>,
        /// Fresh temp local holding the evaluated receiver across the dispatch.
        recv_local: u32,
        /// Fresh temp local for the fallback user method call's own receiver stash.
        fallback_obj_local: u32,
        /// The `TypeError` the user-method fallback throws when the member is
        /// not callable.
        not_callable: CalleeError,
    },
    /// `Promise.resolve(v)`: build a promise already fulfilled with `v`
    /// (or return `v` unchanged when it is itself a promise). Lowers to the host
    /// `__promise` import with [`crate::abi::PROMISE_OP_RESOLVE`].
    PromiseResolve(Box<IrNode>),
    /// `Promise.reject(v)`: build a promise already rejected with reason
    /// `v`. Lowers to the host `__promise` import with
    /// [`crate::abi::PROMISE_OP_REJECT`].
    PromiseReject(Box<IrNode>),
    /// `p.then(onF, onR)` / `p.catch(onR)`: register the reactions on `p`
    /// and yield the chained result promise. A missing handler is `undefined`
    /// (passthrough). Lowers to the host `__then` import.
    PromiseThen {
        /// The receiver promise expression.
        promise: Box<IrNode>,
        /// The fulfillment handler (`undefined` when absent, e.g. `.catch`).
        on_fulfilled: Box<IrNode>,
        /// The rejection handler (`undefined` when absent).
        on_rejected: Box<IrNode>,
    },
    /// `p.finally(fn)`: run `fn` on settlement and pass `p`'s original
    /// settlement through to the chained result promise. Lowers to the host
    /// `__promise` import with [`crate::abi::PROMISE_OP_FINALLY`].
    PromiseFinally {
        /// The receiver promise expression.
        promise: Box<IrNode>,
        /// The finalizer callback, run for its side effect on settlement.
        on_finally: Box<IrNode>,
    },
    /// Top-level `await p`: drive the host microtask queue until `p`
    /// settles, then yield its fulfillment value (or unwind its rejection through
    /// the exception globals). Lowers to the host `__promise` import with
    /// [`crate::abi::PROMISE_OP_AWAIT`]. Only reachable at the top level — an
    /// `await` inside a function body is a parse error (async functions stay
    /// unsupported).
    Await(Box<IrNode>),
    /// `new Promise(executor)`: allocate a fresh pending promise `P`
    /// (stashed in `p_local`), build `resolve`/`reject` FUNCTION values bound to
    /// `P` (via the settler funcref slots), call `executor(resolve, reject)`
    /// synchronously — a throwing executor rejects `P` — and yield `P`. The
    /// `resolve`/`reject` values carry `P` in their env slot, so calling one
    /// drives the host settle logic on `P` (no lexical capture needed).
    PromiseCtor {
        /// The executor expression (a function value taking `(resolve, reject)`).
        executor: Box<IrNode>,
        /// A dedicated local (not scratch) holding `P` across the executor call.
        p_local: u32,
    },
    /// Spawn an `async function` body: the operand is a
    /// [`IrNode::MakeGenerator`] building the handle for the body compiled as a
    /// generator state machine (`await` == `yield`). Lowers to the host
    /// `__promise` import with [`crate::abi::PROMISE_OP_ASYNC_SPAWN`], which
    /// allocates the result promise, drives the first step, and returns the
    /// promise. Emitted as the whole body of the wrapper function the `async`
    /// name binds to (`return AsyncSpawn(MakeGenerator{..})`).
    AsyncSpawn(Box<IrNode>),
    /// `new Date()` / `new Date(ms)`. Allocates a fresh `TYPE_DATE`
    /// block whose f64 payload is the timestamp in milliseconds since the Unix
    /// epoch, and yields the handle heap jsval. `ms` is `None` for the no-argument
    /// form (the payload is the host clock, the `__now` import); otherwise it is
    /// the single constructor argument already wrapped in a `Number(..)` coercion
    /// (an [`IrNode::ConvertCall`] with [`ConvertOp::ToNumber`]), so any operand
    /// stores its numeric value (`new Date("2020")` is `NaN`, like `Number`).
    DateNew {
        /// The millisecond timestamp expression, or `None` for the host clock.
        ms: Option<Box<IrNode>>,
    },
    /// `Date.now()`: the host wall-clock time in milliseconds since the
    /// Unix epoch, as a plain number jsval. Lowers to the host `__now` import.
    DateNow,
    /// `d.getTime()`: the millisecond timestamp of a `TYPE_DATE`
    /// receiver as a number jsval. `recv` is evaluated once; a non-`Date` receiver
    /// yields `NaN` (jsc models no method-receiver `TypeError` here).
    DateGetTime {
        /// The receiver expression (evaluated once).
        recv: Box<IrNode>,
    },
    /// A `Date` instance method `recv.<op>(args)`. Like
    /// [`IrNode::MapMethod`], `recv` is evaluated ONCE into `recv_local`, then a
    /// RUNTIME dispatch on its TYPE BYTE selects the path: a `TYPE_DATE` block
    /// computes the field (`op`) GUEST-side from its stored f64 epoch-ms; any other
    /// receiver falls back to an ordinary user method call `recv.<method_name>(args)`
    /// (binding `this = recv`), so an object/class method of the same name still
    /// works. A non-finite timestamp yields `NaN` (or `"Invalid Date"` for
    /// `toISOString`).
    DateMethod {
        /// Which field the `TYPE_DATE` path computes.
        op: DateOp,
        /// The receiver expression (evaluated once into `recv_local`).
        recv: Box<IrNode>,
        /// The call arguments (Date methods take none, but threaded verbatim to the
        /// user-method fallback so a same-named user method of any arity still works).
        args: Vec<IrNode>,
        /// The source method name, used as the member key of the user-method
        /// fallback and interned as a static string.
        method_name: String,
        /// Fresh temp local holding the evaluated receiver across the dispatch.
        recv_local: u32,
        /// Fresh temp local for the fallback user method call's own receiver stash.
        fallback_obj_local: u32,
        /// The `TypeError` the user-method fallback throws when the member is
        /// not callable.
        not_callable: CalleeError,
    },
}

/// A statement in the numeric-program IR.
#[derive(Debug, Clone, PartialEq)]
pub enum IrStmt {
    /// A `let`/`const` declaration: evaluate `init` and store it in `local`.
    Decl {
        /// The wasm local index the declared variable is bound to.
        local: u32,
        /// The initializer expression.
        init: IrNode,
    },
    /// A bare expression statement.
    Expr(IrNode),
    /// A `return` statement: return from the enclosing function with the given
    /// value, or `None` for a bare `return;` (which yields `NaN` in the
    /// `f64`-only domain, standing in for `undefined`).
    Return(Option<IrNode>),
    /// An `if (cond) { .. } else { .. }` statement. A nonzero `cond` value
    /// selects `then_body`; otherwise `else_body` (empty when there is no
    /// `else`) runs. Bodies share the enclosing function's locals.
    If {
        /// The condition expression (truthy when nonzero).
        cond: IrNode,
        /// Statements run when the condition is truthy.
        then_body: Vec<IrStmt>,
        /// Statements run when the condition is falsy (empty if no `else`).
        else_body: Vec<IrStmt>,
    },
    /// A `while (cond) { .. }` loop. The body runs repeatedly while `cond`
    /// evaluates to a nonzero value, sharing the enclosing function's locals.
    ///
    /// `update` runs after `body` on every iteration AND on a `continue` (which
    /// jumps past the rest of `body` to the update), so a desugared C-style `for`
    /// stores its update expression here rather than appending it to `body` — a
    /// `continue` must still advance the loop variable. A plain `while` leaves it
    /// empty.
    While {
        /// The loop condition (truthy when nonzero).
        cond: IrNode,
        /// Statements run on each iteration.
        body: Vec<IrStmt>,
        /// Statements run after `body` and on every `continue` (the C-style `for`
        /// update); empty for a plain `while`.
        update: Vec<IrStmt>,
        /// The loop's label id when it is the target of a labeled `break`/
        /// `continue` (`Some(id)`), or `None` for an unlabeled loop. A labeled
        /// [`IrStmt::Break`]/[`IrStmt::Continue`] carrying this same id targets
        /// THIS loop even from inside a nested loop.
        label: Option<u32>,
    },
    /// A `switch (disc) { case E: .. default: .. }` statement. The discriminant
    /// is evaluated ONCE into `disc_local`, then each case's test is compared to
    /// it with strict equality (`===`); the first match selects the entry clause
    /// (source order), from which control falls through subsequent clause bodies
    /// until a [`IrStmt::Break`]. `default` (the clause with `test: None`) is the
    /// entry when no case matches, entered at its source position. Clauses share
    /// the enclosing function's locals.
    Switch {
        /// The local the discriminant is stored into (evaluated once).
        disc_local: u32,
        /// The discriminant expression.
        disc: IrNode,
        /// The clauses, in source order.
        cases: Vec<IrCase>,
    },
    /// A `break` statement. `None` targets the nearest enclosing `switch` (or
    /// loop), exiting it; `Some(id)` is a labeled `break LABEL` targeting the
    /// loop whose [`IrStmt::While::label`] equals `id`, even from inside a nested
    /// loop.
    Break(Option<u32>),
    /// A `continue` statement: skip the rest of a loop body and jump to that
    /// loop's update/next-iteration point. `None` targets the nearest enclosing
    /// loop; `Some(id)` is a labeled `continue LABEL` targeting the loop whose
    /// [`IrStmt::While::label`] equals `id`. Only valid inside a loop (not a bare
    /// `switch`). Like `break`, it runs any intervening `finally` clauses on the
    /// way to its target.
    Continue(Option<u32>),
    /// A `try { body } catch (e) { catch_body } finally { finally_body }`
    /// statement. The `body` runs inside an active exception
    /// handler: a `throw` (or a propagated callee exception) reached while it
    /// executes transfers control to `catch_body` (when present) instead of
    /// unwinding out of the function. On a normal (non-throwing) completion of
    /// `body`, `catch_body` is skipped. The catch parameter binding is lowered as
    /// the FIRST statement of `catch_body` — a [`IrStmt::Decl`] whose initializer
    /// reads [`IrNode::ExcValue`] — so this node itself carries no parameter; a
    /// `catch` with no binding simply has no such leading decl.
    ///
    /// `catch_body` is `None` for a `try`/`finally` with no `catch` clause
    /// (distinct from `Some(vec![])`, an empty-but-present `catch`, which still
    /// swallows the exception). `finally_body`, when `Some`, runs on BOTH the
    /// normal fall-through AND every exceptional exit (a `throw` not caught here,
    /// a re-throw from `catch`, or a throw when there is no `catch`); if an
    /// exception is still pending after it, it re-propagates to the next outer
    /// handler / function boundary.
    Try {
        /// The guarded statements, run under the active handler.
        body: Vec<IrStmt>,
        /// The handler statements, run only when `body` threw; the catch
        /// parameter (if any) is bound by a leading [`IrStmt::Decl`]. `None` when
        /// there is no `catch` clause (a `try`/`finally`).
        catch_body: Option<Vec<IrStmt>>,
        /// The `finally` statements, run on every exit path (normal and
        /// exceptional); `None` when there is no `finally` clause.
        finally_body: Option<Vec<IrStmt>>,
    },
}

/// One clause of a [`IrStmt::Switch`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrCase {
    /// The case test expression, compared to the discriminant with `===`; `None`
    /// marks the `default` clause.
    pub test: Option<IrNode>,
    /// The clause body statements, run on entry and via fall-through.
    pub body: Vec<IrStmt>,
}

/// A user-defined function lowered to the numeric IR.
///
/// Under the uniform ABI every function's wasm signature is
/// `(a0..a_{K-1}, env) -> i64`, so the leading `params` locals are the fixed
/// `K` positional argument slots (locals `0..K`, source parameters binding to
/// the first of them) followed by the **env slot** (local `K = params - 1`,
/// the closure environment or `undefined`); the slot count is shared across
/// every function so a single `call_indirect` type serves them all. Any
/// further locals (`params..locals`) are declared inside the body. A source
/// function with fewer than `K` parameters leaves the surplus argument slots
/// unused. The function returns an `f64`, produced via [`IrStmt::Return`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrFunction {
    /// The number of leading wasm parameters (the `K` fixed argument slots plus
    /// the trailing env slot), bound to locals `0..params`.
    pub params: u32,
    /// The total number of `f64` locals (parameters included).
    pub locals: u32,
    /// The function body's statements, in source order. Empty for a generator
    /// state-machine function (its code comes from [`Self::machine`] instead).
    pub body: Vec<IrStmt>,
    /// When `Some`, this function is a generator **state machine**: its
    /// wasm parameters are `(sent, gen, …, env)` under the uniform ABI and its
    /// body is a `switch (gen.state)` dispatch over these `GenSegment`s rather
    /// than [`Self::body`]. `None` for every ordinary function.
    pub machine: Option<Vec<GenSegment>>,
    /// The function object's static facts (its `name`, `length`, and kind).
    pub info: FnInfo,
}

/// The static facts of a function object, laid into the `__fn_info` table
/// and its block's header `flags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FnInfo {
    /// The `name` property (`""` for an anonymous function).
    pub name: String,
    /// The `length` property: the parameters before the first default or rest.
    pub length: u32,
    /// What the function constructs.
    pub kind: FnKind,
}

/// Whether a function is a constructor (design 4.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FnKind {
    /// Not a constructor: arrows, methods, accessors, generators, async
    /// functions, and internal functions. No `prototype`.
    #[default]
    Plain,
    /// A `function` declaration or expression: materializes a `prototype`.
    Constructor,
    /// A class constructor: its `prototype` is defined by the class.
    Class,
    /// A derived class constructor (`class B extends P`): also `SELF_ALLOC`,
    /// so `new` leaves the allocation to `super(..)`.
    DerivedClass,
}

/// One basic block of a generator state machine: a run of
/// straight-line statements ending in a [`GenTerm`]. The block index IS a
/// `gen.state` value; the state machine's `br_table` dispatch enters a block by
/// its index and control flows between blocks by storing the next index into
/// `gen.state` (CFG flattening lets `if`/`while`/`for` cross a `yield`).
#[derive(Debug, Clone, PartialEq)]
pub struct GenSegment {
    /// The statements to run on entry to this block (including any prologue
    /// binding the previous `yield`'s sent value). None contain a `Yield`.
    pub stmts: Vec<IrStmt>,
    /// How the block ends.
    pub term: GenTerm,
    /// The block a throw anywhere in this block (its statements or its
    /// terminator) transfers to — the enclosing `try`'s catch entry, or its
    /// finally-on-throw entry — with the exception still pending. `None` lets the
    /// exception escape the state machine (completing the generator).
    pub handler: Option<u32>,
    /// Whether this block is a handler landing target: it is entered with an
    /// exception pending, which it clears on entry (the thrown value stays
    /// readable through [`IrNode::ExcValue`]).
    pub catches: bool,
}

/// The terminator of a [`GenSegment`].
#[derive(Debug, Clone, PartialEq)]
pub enum GenTerm {
    /// `yield value`: store `resume` into `gen.state`, then return `value` from
    /// the state machine (with `done` left `0`). The next `.next()` re-enters at
    /// block `resume`.
    Yield {
        /// The yielded value.
        value: IrNode,
        /// The block index to resume at on the next `.next()`.
        resume: u32,
    },
    /// A `return value` (or falling off the body end): set `gen.done = 1` and
    /// return `value` (`undefined` when `None`).
    Return(Option<IrNode>),
    /// Unconditional control transfer to block `target`: store `target` into
    /// `gen.state` and re-dispatch.
    Jump(u32),
    /// Conditional transfer: evaluate `cond`; go to block `then_block` when
    /// truthy, else `else_block`.
    Branch {
        /// The condition (truthy → `then_block`).
        cond: IrNode,
        /// The block entered when `cond` is truthy.
        then_block: u32,
        /// The block entered when `cond` is falsy.
        else_block: u32,
    },
}

/// A whole numeric program: a `main` entry function plus any user-defined
/// functions.
///
/// The `locals`/`stmts` fields describe the exported `main` entry function
/// (the top-level program statements). `functions` holds the user-defined
/// functions, whose wasm function indices start at `1` (index `0` is `main`),
/// matching their position in this vector.
#[derive(Debug, Clone, PartialEq)]
pub struct IrProgram {
    /// The number of `f64` locals `main` needs (indices `0..locals`).
    pub locals: u32,
    /// `main`'s statements, in source order.
    pub stmts: Vec<IrStmt>,
    /// The user-defined functions (wasm indices `1..=functions.len()`).
    pub functions: Vec<IrFunction>,
    /// The number of module cells ([`IrNode::ModuleCell`] slots `0..module_cells`).
    pub module_cells: u32,
}

impl IrNode {
    /// An Error instance (design 4.7): a `TYPE_OBJECT` of class
    /// [`crate::abi::CLASS_ERROR`] whose `proto` is `kind`'s static prototype,
    /// with the non-enumerable own properties `name` (only to override the
    /// inherited one, for kinds without a prototype of their own), `message`
    /// (when one was given), and `stack` — the shape every builtin and
    /// engine-raised error shares.
    pub(crate) fn error_object(
        kind: ErrorKind,
        name: Option<&str>,
        message: Option<IrNode>,
        stack: IrNode,
    ) -> Self {
        let name = name.map(|name| {
            (
                PropKey::Hidden("name".to_owned()),
                IrNode::Str(name.to_owned()),
            )
        });
        let message = message.map(|message| (PropKey::Hidden("message".to_owned()), message));
        let stack = (PropKey::Hidden("stack".to_owned()), stack);
        Self::Object {
            entries: name.into_iter().chain(message).chain([stack]).collect(),
            proto: Some(Box::new(IrNode::ErrorProto(kind))),
            class: crate::abi::CLASS_ERROR,
            reserve: 0,
        }
    }

    /// Whether `f` holds for any DIRECT child expression. Exhaustive over every
    /// variant (no wildcard), so a new variant must classify its children before
    /// the tree walkers built on this compile.
    #[allow(clippy::too_many_lines)]
    pub fn any_child(&self, mut f: impl FnMut(&IrNode) -> bool) -> bool {
        match self {
            IrNode::Num(_)
            | IrNode::Str(_)
            | IrNode::Bool(_)
            | IrNode::Null
            | IrNode::Undefined
            | IrNode::ArrayHole
            | IrNode::Local(_)
            | IrNode::EnvLoad(_)
            | IrNode::ModuleCell(_)
            | IrNode::SentValue
            | IrNode::ExcValue
            | IrNode::ThisGlobal
            | IrNode::NewTargetGlobal
            | IrNode::IsConstructCall(_)
            | IrNode::ArgCount(_)
            | IrNode::Hole
            | IrNode::SpreadArgsGlobal
            | IrNode::ArrayPop { .. }
            | IrNode::IncDec { .. }
            | IrNode::MapNew { .. }
            | IrNode::DeclFunc(_)
            | IrNode::ErrorProto(_)
            | IrNode::DateNow => false,
            IrNode::Assign { value: inner, .. }
            | IrNode::CellNew(inner)
            | IrNode::CellGet(inner)
            | IrNode::IsHole(inner)
            | IrNode::IsObject(inner)
            | IrNode::ClassHeritage { value: inner, .. }
            | IrNode::Yield(inner)
            | IrNode::IsGenerator(inner)
            | IrNode::TypeOf(inner)
            | IrNode::StrLen(inner)
            | IrNode::NullishCheck { value: inner, .. }
            | IrNode::ArrayPush { value: inner, .. }
            | IrNode::Not(inner)
            | IrNode::Throw(inner)
            | IrNode::ObjectBuiltin { arg: inner, .. }
            | IrNode::JsonStringify { arg: inner }
            | IrNode::JsonParse { arg: inner }
            | IrNode::PromiseResolve(inner)
            | IrNode::PromiseReject(inner)
            | IrNode::Await(inner)
            | IrNode::PromiseCtor {
                executor: inner, ..
            }
            | IrNode::AsyncSpawn(inner)
            | IrNode::DateGetTime { recv: inner }
            | IrNode::MathSpread { args: inner, .. } => f(inner),
            IrNode::InstanceOf { value, kind } => {
                f(value)
                    || match kind {
                        InstanceOfKind::Proto(rhs) | InstanceOfKind::Ctor(rhs) => f(rhs),
                        InstanceOfKind::Array
                        | InstanceOfKind::Function
                        | InstanceOfKind::Date
                        | InstanceOfKind::Object => false,
                    }
            }
            IrNode::Binary { lhs: a, rhs: b, .. }
            | IrNode::Logical { lhs: a, rhs: b, .. }
            | IrNode::CellSet { cell: a, value: b }
            | IrNode::GenResume { handle: a, sent: b }
            | IrNode::ObjectRest {
                source: a,
                exclude: b,
            }
            | IrNode::Index { obj: a, index: b }
            | IrNode::MakeAccessor {
                getter: a,
                setter: b,
            }
            | IrNode::ArrayHof {
                array: a,
                callback: b,
                init: None,
                ..
            }
            | IrNode::ObjectHas { key: a, obj: b }
            | IrNode::ObjectDelete { obj: a, key: b }
            | IrNode::PromiseFinally {
                promise: a,
                on_finally: b,
            } => f(a) || f(b),
            IrNode::IndexOpSet {
                obj, index, rhs, ..
            } => f(obj) || f(index) || f(rhs),
            IrNode::IndexSet { obj, index, value } => f(obj) || f(index) || f(value),
            IrNode::Ternary {
                cond,
                then_branch,
                else_branch,
            } => f(cond) || f(then_branch) || f(else_branch),
            IrNode::PromiseThen {
                promise,
                on_fulfilled,
                on_rejected,
            } => f(promise) || f(on_fulfilled) || f(on_rejected),
            IrNode::ArrayHof {
                array,
                callback,
                init: Some(init),
                ..
            } => f(array) || f(callback) || f(init),
            IrNode::ConvertCall { arg, radix, .. } => f(arg) || radix.as_deref().is_some_and(f),
            IrNode::DateNew { ms } => ms.as_deref().is_some_and(f),
            IrNode::Call { args, .. }
            | IrNode::FuncValue { captures: args, .. }
            | IrNode::MakeGenerator { slots: args, .. }
            | IrNode::Array(args)
            | IrNode::Seq(args)
            | IrNode::ConsoleLog { args }
            | IrNode::MathCall { args, .. } => args.iter().any(f),
            IrNode::CallIndirect {
                callee: recv, args, ..
            }
            | IrNode::MethodCall { recv, args, .. }
            | IrNode::ThisCall {
                callee: recv, args, ..
            }
            | IrNode::MapMethod { recv, args, .. }
            | IrNode::ReceiverMethod { recv, args, .. }
            | IrNode::DateMethod { recv, args, .. } => f(recv) || args.iter().any(f),
            IrNode::MethodCallUser { obj, key, args, .. } => f(obj) || f(key) || args.iter().any(f),
            IrNode::Construct {
                callee,
                args,
                spread,
                new_target,
                ..
            } => {
                f(callee)
                    || args.iter().any(&mut f)
                    || spread.as_deref().is_some_and(&mut f)
                    || new_target.as_deref().is_some_and(f)
            }
            IrNode::SetProto { obj, proto } => f(obj) || f(proto),
            IrNode::Object { entries, proto, .. } => {
                proto.as_deref().is_some_and(&mut f)
                    || entries.iter().any(|(key, value)| {
                        matches!(key, PropKey::Computed(key) if f(key)) || f(value)
                    })
            }
            IrNode::DefineProps { obj, entries, .. } => {
                f(obj)
                    || entries.iter().any(|(key, value)| {
                        matches!(key, PropKey::Computed(key) if f(key)) || f(value)
                    })
            }
            IrNode::ArraySpread(elems) => elems.iter().any(|elem| match elem {
                SpreadElem::Item(value) | SpreadElem::Spread(value) => f(value),
            }),
            IrNode::CallSpread { target, args } => {
                matches!(target, SpreadCallee::Indirect(callee, _) | SpreadCallee::This { callee, .. } | SpreadCallee::Method { callee, .. } if f(callee))
                    || f(args)
            }
        }
    }

    /// Mutable references to every DIRECT child expression, in JS evaluation
    /// order (the order [`Self::any_child`] visits them). Exhaustive over every
    /// variant, like [`Self::any_child`].
    #[allow(clippy::too_many_lines)]
    pub fn children_mut(&mut self) -> Vec<&mut IrNode> {
        match self {
            IrNode::Num(_)
            | IrNode::Str(_)
            | IrNode::Bool(_)
            | IrNode::Null
            | IrNode::Undefined
            | IrNode::ArrayHole
            | IrNode::Local(_)
            | IrNode::EnvLoad(_)
            | IrNode::ModuleCell(_)
            | IrNode::SentValue
            | IrNode::ExcValue
            | IrNode::ThisGlobal
            | IrNode::NewTargetGlobal
            | IrNode::IsConstructCall(_)
            | IrNode::ArgCount(_)
            | IrNode::Hole
            | IrNode::SpreadArgsGlobal
            | IrNode::ArrayPop { .. }
            | IrNode::IncDec { .. }
            | IrNode::MapNew { .. }
            | IrNode::DeclFunc(_)
            | IrNode::ErrorProto(_)
            | IrNode::DateNow => Vec::new(),
            IrNode::Assign { value: inner, .. }
            | IrNode::CellNew(inner)
            | IrNode::CellGet(inner)
            | IrNode::IsHole(inner)
            | IrNode::IsObject(inner)
            | IrNode::ClassHeritage { value: inner, .. }
            | IrNode::Yield(inner)
            | IrNode::IsGenerator(inner)
            | IrNode::TypeOf(inner)
            | IrNode::StrLen(inner)
            | IrNode::NullishCheck { value: inner, .. }
            | IrNode::ArrayPush { value: inner, .. }
            | IrNode::Not(inner)
            | IrNode::Throw(inner)
            | IrNode::ObjectBuiltin { arg: inner, .. }
            | IrNode::JsonStringify { arg: inner }
            | IrNode::JsonParse { arg: inner }
            | IrNode::PromiseResolve(inner)
            | IrNode::PromiseReject(inner)
            | IrNode::Await(inner)
            | IrNode::PromiseCtor {
                executor: inner, ..
            }
            | IrNode::AsyncSpawn(inner)
            | IrNode::DateGetTime { recv: inner }
            | IrNode::MathSpread { args: inner, .. } => vec![&mut **inner],
            IrNode::InstanceOf { value, kind } => match kind {
                InstanceOfKind::Proto(rhs) | InstanceOfKind::Ctor(rhs) => {
                    vec![&mut **value, &mut **rhs]
                }
                InstanceOfKind::Array
                | InstanceOfKind::Function
                | InstanceOfKind::Date
                | InstanceOfKind::Object => vec![&mut **value],
            },
            IrNode::Binary { lhs: a, rhs: b, .. }
            | IrNode::Logical { lhs: a, rhs: b, .. }
            | IrNode::CellSet { cell: a, value: b }
            | IrNode::GenResume { handle: a, sent: b }
            | IrNode::ObjectRest {
                source: a,
                exclude: b,
            }
            | IrNode::Index { obj: a, index: b }
            | IrNode::MakeAccessor {
                getter: a,
                setter: b,
            }
            | IrNode::ArrayHof {
                array: a,
                callback: b,
                init: None,
                ..
            }
            | IrNode::ObjectHas { key: a, obj: b }
            | IrNode::ObjectDelete { obj: a, key: b }
            | IrNode::PromiseFinally {
                promise: a,
                on_finally: b,
            } => vec![&mut **a, &mut **b],
            IrNode::IndexOpSet {
                obj, index, rhs, ..
            } => vec![&mut **obj, &mut **index, &mut **rhs],
            IrNode::IndexSet { obj, index, value } => vec![&mut **obj, &mut **index, &mut **value],
            IrNode::Ternary {
                cond,
                then_branch,
                else_branch,
            } => vec![&mut **cond, &mut **then_branch, &mut **else_branch],
            IrNode::PromiseThen {
                promise,
                on_fulfilled,
                on_rejected,
            } => vec![&mut **promise, &mut **on_fulfilled, &mut **on_rejected],
            IrNode::ArrayHof {
                array,
                callback,
                init: Some(init),
                ..
            } => vec![&mut **array, &mut **callback, &mut **init],
            IrNode::ConvertCall { arg, radix, .. } => {
                let mut out = vec![&mut **arg];
                out.extend(radix.as_deref_mut());
                out
            }
            IrNode::DateNew { ms } => ms.as_deref_mut().into_iter().collect(),
            IrNode::Call { args, .. }
            | IrNode::FuncValue { captures: args, .. }
            | IrNode::MakeGenerator { slots: args, .. }
            | IrNode::Array(args)
            | IrNode::Seq(args)
            | IrNode::ConsoleLog { args }
            | IrNode::MathCall { args, .. } => args.iter_mut().collect(),
            IrNode::CallIndirect {
                callee: recv, args, ..
            }
            | IrNode::MethodCall { recv, args, .. }
            | IrNode::ThisCall {
                callee: recv, args, ..
            }
            | IrNode::MapMethod { recv, args, .. }
            | IrNode::ReceiverMethod { recv, args, .. }
            | IrNode::DateMethod { recv, args, .. } => std::iter::once(&mut **recv)
                .chain(args.iter_mut())
                .collect(),
            IrNode::MethodCallUser { obj, key, args, .. } => [&mut **obj, &mut **key]
                .into_iter()
                .chain(args.iter_mut())
                .collect(),
            IrNode::Construct {
                callee,
                args,
                spread,
                new_target,
                ..
            } => std::iter::once(&mut **callee)
                .chain(args.iter_mut())
                .chain(spread.as_deref_mut())
                .chain(new_target.as_deref_mut())
                .collect(),
            IrNode::SetProto { obj, proto } => vec![&mut **obj, &mut **proto],
            IrNode::Object { entries, proto, .. } => {
                let mut out: Vec<&mut IrNode> = proto.as_deref_mut().into_iter().collect();
                for (key, value) in entries {
                    if let PropKey::Computed(key) = key {
                        out.push(key);
                    }
                    out.push(value);
                }
                out
            }
            IrNode::DefineProps { obj, entries, .. } => {
                let mut out: Vec<&mut IrNode> = vec![&mut **obj];
                for (key, value) in entries {
                    if let PropKey::Computed(key) = key {
                        out.push(key);
                    }
                    out.push(value);
                }
                out
            }
            IrNode::ArraySpread(elems) => elems
                .iter_mut()
                .map(|elem| match elem {
                    SpreadElem::Item(value) | SpreadElem::Spread(value) => value,
                })
                .collect(),
            IrNode::CallSpread { target, args } => {
                let mut out = Vec::new();
                if let SpreadCallee::Indirect(callee, _)
                | SpreadCallee::This { callee, .. }
                | SpreadCallee::Method { callee, .. } = target
                {
                    out.push(&mut **callee);
                }
                out.push(&mut **args);
                out
            }
        }
    }
}

impl IrStmt {
    /// Whether `f` holds for any DIRECT child expression (not descending into
    /// nested statements). Exhaustive over every variant, like
    /// [`IrNode::any_child`].
    pub fn any_node(&self, mut f: impl FnMut(&IrNode) -> bool) -> bool {
        match self {
            IrStmt::Decl { init: node, .. }
            | IrStmt::Expr(node)
            | IrStmt::If { cond: node, .. }
            | IrStmt::While { cond: node, .. } => f(node),
            IrStmt::Return(value) => value.as_ref().is_some_and(f),
            IrStmt::Switch { disc, cases, .. } => {
                f(disc)
                    || cases
                        .iter()
                        .any(|case| case.test.as_ref().is_some_and(&mut f))
            }
            IrStmt::Break(_) | IrStmt::Continue(_) | IrStmt::Try { .. } => false,
        }
    }

    /// Whether `f` holds for any DIRECT child statement (one nesting level).
    /// Exhaustive over every variant, like [`IrNode::any_child`].
    pub fn any_stmt(&self, f: impl FnMut(&IrStmt) -> bool) -> bool {
        match self {
            IrStmt::Decl { .. }
            | IrStmt::Expr(_)
            | IrStmt::Return(_)
            | IrStmt::Break(_)
            | IrStmt::Continue(_) => false,
            IrStmt::If {
                then_body: a,
                else_body: b,
                ..
            }
            | IrStmt::While {
                body: a, update: b, ..
            } => a.iter().chain(b).any(f),
            IrStmt::Switch { cases, .. } => cases.iter().flat_map(|case| &case.body).any(f),
            IrStmt::Try {
                body,
                catch_body,
                finally_body,
            } => body
                .iter()
                .chain(catch_body.iter().flatten())
                .chain(finally_body.iter().flatten())
                .any(f),
        }
    }
}

#[cfg(test)]
mod tests;
