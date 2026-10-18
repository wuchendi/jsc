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
    /// in insertion order (reserved hidden `@@`-prefixed keys skipped).
    Keys,
    /// `Object.values(o)` — a NEW array of the object's own enumerable values, in
    /// insertion order (reserved hidden `@@`-prefixed keys skipped).
    Values,
    /// `Object.entries(o)` — a NEW array of `[key, value]` two-element arrays, one
    /// per own enumerable entry in insertion order (reserved keys skipped).
    Entries,
}

/// What the right-hand side of an `instanceof` names (E-series). Recognized
/// syntactically at codegen from a bare identifier: a **user class** (the
/// receiver's `@@prototype` chain is walked for the class's shared prototype)
/// takes precedence, so `class Error {}` shadows the builtin; otherwise the three
/// builtin error constructors match the receiver's hidden brand (`Error` matches
/// any branded error; `TypeError`/`RangeError` match only their own kind, both
/// still `instanceof Error`). Any other right-hand side is rejected at lowering.
///
/// The builtin constructors `Array`/`Object`/`Function`/`Date` match by the
/// receiver's runtime TYPE BYTE rather than a prototype hop: `Array` /
/// `Function` / `Date` match their own type byte, and `Object` matches ANY heap
/// pointer that is not a primitive string (so arrays, plain objects, functions,
/// maps, sets, promises, generators, errors, and dates are all `instanceof
/// Object`, while a string/number/boolean/null/undefined is not).
#[derive(Debug, Clone, PartialEq)]
pub enum InstanceOfKind {
    /// `instanceof Error` — matches any branded error (marker present).
    Error,
    /// `instanceof TypeError` — brand `=== ERROR_KIND_TYPE`.
    TypeError,
    /// `instanceof RangeError` — brand `=== ERROR_KIND_RANGE`.
    RangeError,
    /// `instanceof Array` — the receiver is a heap `TYPE_ARRAY` block.
    Array,
    /// `instanceof Function` — the receiver is a heap `TYPE_FUNCTION` value.
    Function,
    /// `instanceof Date` — the receiver is a heap `TYPE_DATE` block.
    Date,
    /// `instanceof Object` — the receiver is any heap pointer that is not a
    /// primitive string.
    Object,
    /// `instanceof C` for a user class `C`: some object on the receiver's
    /// `@@prototype` chain must be the SAME object as `C`'s shared prototype. The
    /// walk starts at the receiver's own `@@prototype` and follows each prototype
    /// object's own `@@prototype` link (installed by `extends`) until a match or the
    /// base class (no link). Carries the expression that reads the class's prototype
    /// (`C["@@prototype"]`). Under no-`extends` the chain is one hop.
    Class(Box<IrNode>),
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
    /// An indirect call through a computed function value.
    Indirect(Box<IrNode>),
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
    },
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
    },
    /// The unary `typeof` operator applied to a sub-expression. It yields a
    /// boxed type-string immediate (tag `0b100`) naming the operand's runtime
    /// type (`"number"`/`"boolean"`/`"object"`/`"undefined"`).
    TypeOf(Box<IrNode>),
    /// An array literal `[e0, e1, ..]`. It lowers to a heap array (single heap tag
    /// `0b101`, type byte `1`): `__alloc(HEADER_SIZE + 4 + 8*n)`, the type byte, a
    /// u32 `len` header of `n`, then each element jsval stored at
    /// `ptr + HEADER_SIZE + 4 + 8*k`.
    /// Elements are evaluated left-to-right.
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
    /// slots (padding with `undefined`, dropping any beyond `K`) and passes the
    /// array's length as the dynamic `argc`.
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
    /// An object literal `{k0: e0, k1: e1, ..}` with compile-time string keys. It
    /// lowers to a heap object (single heap tag `0b101`, type byte `2`):
    /// `__alloc(HEADER_SIZE + 4 + 16*n)`, the type byte, a u32 `n` header, then
    /// each entry's key heap-string pointer at `ptr + HEADER_SIZE + 4 + 16*k` and
    /// value jsval at `+8`. Keys are
    /// interned as static strings; value expressions are evaluated left-to-right.
    Object(Vec<(String, IrNode)>),
    /// An object rest binding `let {a, ...rest} = obj`. Builds a NEW heap
    /// object holding every own key of `source` EXCEPT the statically-known
    /// destructured keys in `exclude`. `source` is evaluated once (a hidden temp
    /// read); `exclude` is an [`IrNode::Array`] of [`IrNode::Str`] key names. It
    /// lowers to the `__obj_rest` runtime helper, which linear-scans the source
    /// object's entries, skips any key byte-equal to an excluded one, copies the
    /// rest into a fresh bump-allocated object, and returns it as a heap jsval.
    ObjectRest {
        /// The source object being spread (a heap object jsval, evaluated once).
        source: Box<IrNode>,
        /// The excluded keys — an [`IrNode::Array`] of [`IrNode::Str`] names.
        exclude: Box<IrNode>,
    },
    /// The `.length` member of a string or array: reads the u32 `len` header at
    /// the operand's block base `+ HEADER_SIZE` and produces it as a number jsval.
    /// The operand is assumed to be a string or array pointer (both
    /// carry the same u32 header at that offset).
    StrLen(Box<IrNode>),
    /// The nullish-receiver guard of a builtin member access (`recv.length`,
    /// `recv.push(..)`, `recv.get(..)`, ...): yields `value` unchanged, but throws
    /// the branded `TypeError` `Cannot read properties of <null|undefined>
    /// (reading '<key>')` when it is `null`/`undefined`. Wrapped around the
    /// receiver by codegen; elided at emit for a statically non-nullish value.
    NullishCheck {
        /// The receiver expression being guarded.
        value: Box<IrNode>,
        /// The property name the access reads (for the error message).
        key: String,
    },
    /// Computed indexing `obj[index]`, dispatched at runtime on `obj`'s TYPE BYTE.
    /// A string block (type `0`) allocates and returns a fresh
    /// one-character heap string holding the BYTE at `index`; an array block (type
    /// `1`) loads and returns the element jsval at `ptr + HEADER_SIZE + 4 +
    /// 8*index`; an object block (type `2`) linear-scans its entries for a key
    /// whose byte content equals the string `index`, returning the matching value
    /// jsval or `undefined`. Static property reads `obj.a` also lower here as
    /// `obj["a"]`. For string/array, `index` is a number jsval assumed in bounds
    /// (`0 <= index < length`); for objects, `index` is a heap-string pointer.
    Index {
        /// The string or array being indexed (a heap-pointer jsval).
        obj: Box<IrNode>,
        /// The index (a number jsval).
        index: Box<IrNode>,
    },
    /// A class instance **accessor** property value (P2b getters/setters): build a
    /// `TYPE_ACCESSOR` heap block holding the getter and setter FUNCTION VALUES
    /// (`undefined` for an absent half). It is stored under the property name on
    /// the class's shared `@@prototype` (alongside the plain method values), so a
    /// member READ that resolves to this box invokes `getter()` with `this` = the
    /// receiver, and a member WRITE invokes `setter(value)`. Both halves are
    /// ordinary lifted function values that read `this` through the `this` carrier,
    /// exactly like a method. Built ONCE per accessor property at class-definition
    /// time — never copied per instance.
    MakeAccessor {
        /// The getter function value, or [`IrNode::Undefined`] if the property
        /// declares only a setter.
        getter: Box<IrNode>,
        /// The setter function value, or [`IrNode::Undefined`] if the property
        /// declares only a getter.
        setter: Box<IrNode>,
    },
    /// Computed element assignment `obj[index] = value`. It stores `value`
    /// into the EXISTING slot at `ptr + HEADER_SIZE + 4 + 8*index` of the array
    /// `obj` and yields `value` (JS: `a[i] = v` evaluates to `v`). `obj` is assumed
    /// to be an array block (type byte `1`) and `index` a number in bounds — no
    /// bounds check and no growth. An object
    /// target (type byte `2`) dispatches to the key-scan store instead.
    IndexSet {
        /// The array being written (a heap array-pointer jsval).
        obj: Box<IrNode>,
        /// The index (a number jsval).
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
    /// `array.push(value)` on a plain-local array binding. Growing the
    /// fixed-size array block: a runtime helper allocates a new block of `len + 1`
    /// capacity, copies the header + existing elements, appends `value`, and bumps
    /// the length; the `local` is then REBOUND (`local.set`) to the new block base
    /// so subsequent reads / `.length` observe the growth. Yields the new length as
    /// a number jsval (JS semantics). Only a plain local receiver is supported — a
    /// captured (boxed-cell) array is rejected at codegen.
    ArrayPush {
        /// The wasm local holding the array; read for the old block, rebound to the
        /// reallocated one.
        local: u32,
        /// The value appended at the new last slot.
        value: Box<IrNode>,
    },
    /// `array.pop()` on a plain-local array binding. Shrinks the block in
    /// place: the u32 length header is decremented and the removed last element
    /// jsval is yielded (no realloc, the local keeps its pointer). An empty array
    /// yields `undefined`.
    ArrayPop {
        /// The wasm local holding the array whose block is decremented in place.
        local: u32,
    },
    /// An array higher-order method call `array.map/filter/reduce/forEach(...)`.
    /// The `array` receiver is evaluated once into a scratch base (it is
    /// NOT rebound — unlike [`IrNode::ArrayPush`] — so any array expression works),
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
    /// argument. `Boolean` lowers to inline JS-truthiness wasm; the other four
    /// delegate to the host `__convert` import, which reuses the run stage's
    /// `format_value`/parse helpers (so `String(42) == "42"`, matching
    /// `format_number`).
    ConvertCall {
        /// Which conversion this call performs.
        op: ConvertOp,
        /// The single argument expression, evaluated once to a jsval.
        arg: Box<IrNode>,
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
    /// value at member `key` of `obj` (an object read reusing the `__obj_get`
    /// path); the `__this` global is set to `obj` immediately before the
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
    },
    /// Read the `__this` wasm global. Emitted ONLY in a function/arrow
    /// prologue to snapshot the current `this` into the function's local `this`
    /// slot, so a nested method call that re-sets the global does not disturb this
    /// frame's already-captured `this`.
    ThisGlobal,
    /// A `super(..)` / `super.m(..)` call inside a derived class (the E-series
    /// `extends`): invoke the function value `callee` with `this` kept as the
    /// CURRENT instance (the enclosing method's snapshotted `this` in `this_local`)
    /// rather than a fresh receiver. `callee` is the parent constructor
    /// (`Super["@@constructor"]`) for `super(..)` or a parent-prototype method
    /// (`Super["@@prototype"].m`) for `super.m(..)`. Mirrors [`IrNode::MethodCallUser`]'s
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
    },
    /// A class instantiation `new A(args)`. Allocates the fresh `instance`
    /// object (pre-shaped with the reserved prototype link plus one slot per field
    /// the constructor assigns via `this.<field> = ..`, since the object model has
    /// no dynamic key growth), binds `this = instance`, runs the constructor
    /// function value `ctor` with `args` under the uniform ABI, DISCARDS the
    /// constructor's return value, and evaluates to the instance. The instance is
    /// stashed in `instance_local` so it survives the constructor call and is
    /// yielded afterward — mirroring [`IrNode::MethodCallUser`]'s `this`-binding
    /// call shape but yielding the receiver rather than the call result.
    New {
        /// The fresh instance allocation: an [`IrNode::Object`] whose first entry
        /// is the reserved prototype-key link and whose remaining entries slot
        /// each constructor-assigned field to `undefined`.
        instance: Box<IrNode>,
        /// The constructor function value to run (read from the class binding's
        /// reserved constructor key).
        ctor: Box<IrNode>,
        /// The constructor argument expressions, in source order.
        args: Vec<IrNode>,
        /// Fresh temp local holding the instance across the constructor call.
        instance_local: u32,
    },
    /// The enumerable KEY at position `index` of `obj`, produced as a heap STRING
    /// jsval. This is the key-table iteration primitive `for-in` desugars
    /// to (and that `Object.keys` / `JSON.stringify` reuse): a counted
    /// loop over `0 <= index < StrLen(obj)` reads each key through this node.
    /// Dispatches at RUNTIME on `obj`'s TYPE BYTE: an object
    /// (`TYPE_OBJECT`) returns the key heap-string jsval already stored at entry
    /// `index` (`ptr + HEADER_SIZE + 4 + 16*index`), in insertion order; anything
    /// else (an array, or a string) treats `index` as a numeric position and
    /// yields its decimal string form (`"0"`, `"1"`, ..) via the host `__convert`
    /// to-string op, matching JS `for-in` index keys. `index` is a number jsval
    /// assumed in range (the loop bounds it against the length).
    ObjectKey {
        /// The object/array being iterated (a heap-pointer jsval).
        obj: Box<IrNode>,
        /// The position of the key to read (a number jsval, `0..length`).
        index: Box<IrNode>,
    },
    /// An `Object.keys/values/entries(arg)` collection built-in call. The
    /// `arg` object is evaluated once to a heap-object jsval, then a runtime helper
    /// (`__obj_keys`/`__obj_values`/`__obj_entries`) linear-scans its key table —
    /// skipping the reserved hidden `@@`-prefixed keys — and builds a fresh growable
    /// array of the keys, the values, or `[key, value]` pairs, returned as a heap
    /// jsval.
    ObjectBuiltin {
        /// Which `Object.*` collection method this call performs.
        op: ObjectBuiltinOp,
        /// The single object argument, evaluated once to a heap-object jsval.
        arg: Box<IrNode>,
    },
    /// A `key in obj` membership test, yielding a `true`/`false` jsval. Both
    /// operands are evaluated once to heap jsvals; a runtime helper (`__obj_has`)
    /// linear-scans `obj`'s own key table for a key whose byte content equals the
    /// `key` heap-string (own keys only — the prototype chain is NOT walked).
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
    /// test: a user class compares the receiver's `@@prototype` against the
    /// class's; a builtin error checks the hidden [`crate::abi::ERROR_KIND_KEY`]
    /// brand. The right-hand side is restricted at lowering to a user class or
    /// one of the three builtin error constructors (no general prototype-chain
    /// `instanceof`).
    InstanceOf {
        /// The left operand (any value).
        value: Box<IrNode>,
        /// What the right-hand side names.
        kind: InstanceOfKind,
    },
    /// A `delete obj.k` property removal, always yielding a `true` jsval.
    /// Both operands are evaluated once to heap jsvals; a runtime helper
    /// (`__obj_delete`) linear-scans `obj`'s own key table for the `key`
    /// heap-string and, on a match, compacts the entry out (shifts the trailing
    /// entries down and decrements the `n` header) so a later read yields
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
    /// objects recurse (object keys use the runtime key table, skipping reserved
    /// `@@`-prefixed keys; `undefined`/function values are omitted as object values
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
    /// guest value tree — including OBJECTS with **runtime-unknown keys**, built by
    /// allocating a fresh entry table sized to the parsed member count (the
    /// fixed-shape object model only blocks *mutating* keys via `__obj_set`, not
    /// allocating a fresh object; and every consumer matches keys by byte content,
    /// so runtime heap-string keys resolve against interned literal keys). Malformed
    /// input throws a branded `Error` (`.name` `SyntaxError`). The optional reviver
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

#[cfg(test)]
mod tests;
