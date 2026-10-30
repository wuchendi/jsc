#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit a binary operation, leaving its jsval result on the stack. Arithmetic
/// unboxes to `f64` and reboxes a number; relational operators box a boolean
/// jsval; `Eq`/`Ne` are strict equality (see [`emit_strict_eq`]).
/// Emit an increment/decrement of `local` (`++`/`--`). The local is first
/// replaced by its `ToNumber`, then the updated value is `old ± 1` (unboxed to
/// `f64`, stepped, reboxed). `prefix` yields the NEW value via `local.tee`;
/// postfix pushes the (numeric) OLD value first and then stores the update, so
/// the old value remains on the stack as the expression's result.
pub(super) fn emit_inc_dec(
    local: u32,
    inc: bool,
    prefix: bool,
    function: &mut Function,
    helpers: Helpers<'_>,
) {
    let step = if inc {
        Instruction::F64Add
    } else {
        Instruction::F64Sub
    };
    emit_coerce_number(function, local, helpers);
    if prefix {
        // new = old ± 1; store and yield the new value.
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        function.instruction(&step);
        emit_box_f64(function, helpers);
        function.instruction(&Instruction::LocalTee(local));
    } else {
        // Yield the old value (left on the stack), then store `old ± 1`.
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        function.instruction(&step);
        emit_box_f64(function, helpers);
        function.instruction(&Instruction::LocalSet(local));
    }
}

/// Emit the numeric remainder `lhs % rhs`. wasm has no `f64.rem`, so compute the
/// JS truncated remainder `a - b * trunc(a / b)` (which takes the dividend's
/// sign). Both operands are stashed in the scratch locals (coerced with
/// `ToNumber`) so each can be read twice; the `f64` result is reboxed as a number
/// jsval.
pub(super) fn emit_mod(
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s0 = scratch;
    let s1 = scratch + 1;
    emit_node(lhs, function, scratch, strings, helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));
    emit_coerce_number(function, s0, helpers);
    emit_coerce_number(function, s1, helpers);
    // a
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::F64ReinterpretI64);
    // b * trunc(a / b)
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Div);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Mul);
    // a - b * trunc(a / b)
    function.instruction(&Instruction::F64Sub);
    emit_box_f64(function, helpers);
}

/// Emit subtraction/multiplication/division: stash both operands, coerce each
/// with `ToNumber`, unbox to `f64`, compute, rebox as a number.
fn emit_arith(
    op: BinOp,
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s0 = scratch;
    let s1 = scratch + 1;
    emit_node(lhs, function, scratch, strings, helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));
    emit_coerce_number(function, s0, helpers);
    emit_coerce_number(function, s1, helpers);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    let arith = match op {
        BinOp::Sub => Instruction::F64Sub,
        BinOp::Mul => Instruction::F64Mul,
        BinOp::Div => Instruction::F64Div,
        _ => unreachable!("only sub/mul/div reach here"),
    };
    function.instruction(&arith);
    emit_box_f64(function, helpers);
}

/// Emit a bitwise or shift operator (`& | ^ << >> >>>`) over two jsval operands,
/// leaving a number jsval on the stack.
///
/// Both operands are evaluated left to right, stashed, and coerced with
/// `ToNumber` in order (a user `valueOf` on the left runs before the right's),
/// then each is converted with [`emit_to_int32`]. The wasm shifts already mask
/// the count to its low 5 bits (`ToUint32(rhs) & 31`). `>>>` reads its result
/// as unsigned; every other operator as signed.
fn emit_bitwise(
    op: BinOp,
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s0 = scratch;
    let s1 = scratch + 1;
    emit_node(lhs, function, scratch, strings, helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));
    emit_coerce_number(function, s0, helpers);
    emit_coerce_number(function, s1, helpers);
    emit_to_int32(function, s0);
    emit_to_int32(function, s1);
    let (int_op, convert) = match op {
        BinOp::BitAnd => (Instruction::I32And, Instruction::F64ConvertI32S),
        BinOp::BitOr => (Instruction::I32Or, Instruction::F64ConvertI32S),
        BinOp::BitXor => (Instruction::I32Xor, Instruction::F64ConvertI32S),
        BinOp::Shl => (Instruction::I32Shl, Instruction::F64ConvertI32S),
        BinOp::Shr => (Instruction::I32ShrS, Instruction::F64ConvertI32S),
        BinOp::UShr => (Instruction::I32ShrU, Instruction::F64ConvertI32U),
        _ => unreachable!("only bitwise/shift ops reach here"),
    };
    function.instruction(&int_op);
    function.instruction(&convert);
    // An integer conversion is never NaN, so it reinterprets directly.
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Push the 32-bit pattern of `ToInt32` (equivalently `ToUint32`) of the number
/// jsval in `local` as an i32.
///
/// Computes `m = t - 2^32 * floor(t / 2^32)` with `t = trunc(x)`. Every step is
/// exact for any finite `x` (scaling by a power of two is exact, and `m` is an
/// integer in `[0, 2^32)` that is a multiple of `ulp(t)`), so magnitudes beyond
/// the i64 range still wrap correctly. NaN and ±Infinity make `m` NaN, and `-0`
/// makes it `+0`; the saturating unsigned truncation maps both to `0` and
/// reinterprets `[2^31, 2^32)` as the negative int32 range.
fn emit_to_int32(function: &mut Function, local: u32) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0 / 4_294_967_296.0)));
    function.instruction(&Instruction::F64Mul);
    function.instruction(&Instruction::F64Floor);
    function.instruction(&Instruction::F64Const(Ieee64::from(4_294_967_296.0)));
    function.instruction(&Instruction::F64Mul);
    function.instruction(&Instruction::F64Sub);
    function.instruction(&Instruction::I32TruncSatF64U);
}

/// Emit a `Math.*` built-in call, leaving its number-jsval result on the stack.
///
/// Every argument is converted with `ToNumber` (a user `valueOf`/`toString`
/// runs, and may throw); the conversion is emitted behind one `is_number` test
/// and skipped for an argument that always yields a number. The unary ops then
/// unbox their single argument to `f64`, apply a native wasm op (or the host
/// `__method` [`abi::METHOD_OP_MATH`] function for the transcendentals), and
/// rebox. `Min`/`Max` fold the native `f64.min`/`f64.max` (which propagate NaN
/// and order `-0`/`+0` like JS) over the arguments starting from the identity
/// (`+Infinity` for min, `-Infinity` for max), so the empty-args case yields
/// the correct infinity; `Hypot` folds its scaled sum of squares the same way
/// ([`emit_math_spread`]). `Pow` delegates to [`emit_pow`]; the binary
/// `Atan2` / `Imul` evaluate both arguments before converting either.
#[allow(clippy::too_many_lines)] // one flat dispatch over the methods
pub(super) fn emit_math_call(
    op: MathOp,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let x = |function: &mut Function, local: u32| {
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
    };
    match op {
        MathOp::Min | MathOp::Max => emit_min_max(op, args, function, scratch, strings, helpers),
        MathOp::Pow => emit_pow(&args[0], &args[1], function, scratch, strings, helpers),
        MathOp::Hypot => {
            let items = args.iter().cloned().map(SpreadElem::Item).collect();
            let spread = IrNode::ArraySpread(items);
            emit_math_spread(op, &spread, function, scratch, strings, helpers);
        }
        MathOp::Random => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            emit_host_math(abi::MATH_FN_RANDOM, None, function, helpers);
        }
        MathOp::Atan2 | MathOp::Imul => {
            let (s0, s1) = (scratch, scratch + 1);
            emit_node(&args[0], function, scratch, strings, helpers);
            emit_node(&args[1], function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(s1));
            function.instruction(&Instruction::LocalSet(s0));
            emit_coerce_number(function, s0, helpers);
            emit_coerce_number(function, s1, helpers);
            if op == MathOp::Atan2 {
                function.instruction(&Instruction::LocalGet(s0));
                emit_host_math(abi::MATH_FN_ATAN2, Some(s1), function, helpers);
            } else {
                emit_to_int32(function, s0);
                emit_to_int32(function, s1);
                function.instruction(&Instruction::I32Mul);
                function.instruction(&Instruction::F64ConvertI32S);
                // An int32 is never NaN, so it reinterprets directly.
                function.instruction(&Instruction::I64ReinterpretF64);
            }
        }
        // Unary methods: ToNumber → unbox → op → rebox.
        _ => {
            let native = match op {
                MathOp::Floor => Some(Instruction::F64Floor),
                MathOp::Ceil => Some(Instruction::F64Ceil),
                MathOp::Trunc => Some(Instruction::F64Trunc),
                MathOp::Abs => Some(Instruction::F64Abs),
                MathOp::Sqrt => Some(Instruction::F64Sqrt),
                _ => None,
            };
            emit_node(&args[0], function, scratch, strings, helpers);
            // A single-use number operand stays on the stack.
            let stashed = !(yields_number(&args[0]) && (native.is_some() || op == MathOp::Fround));
            if stashed {
                function.instruction(&Instruction::LocalSet(scratch));
                if !yields_number(&args[0]) {
                    emit_coerce_number(function, scratch, helpers);
                }
            }
            if let Some(code) = host_math_code(op) {
                function.instruction(&Instruction::LocalGet(scratch));
                emit_host_math(code, None, function, helpers);
                return;
            }
            if op == MathOp::Clz32 {
                emit_to_int32(function, scratch);
                function.instruction(&Instruction::I32Clz);
                function.instruction(&Instruction::F64ConvertI32U);
                function.instruction(&Instruction::I64ReinterpretF64);
                return;
            }
            if stashed {
                function.instruction(&Instruction::LocalGet(scratch));
            }
            function.instruction(&Instruction::F64ReinterpretI64);
            if let Some(native) = native {
                function.instruction(&native);
            } else if op == MathOp::Fround {
                function.instruction(&Instruction::F32DemoteF64);
                function.instruction(&Instruction::F64PromoteF32);
            } else if op == MathOp::Round {
                // JS `round(x)`: `r = floor(x)`, plus one when `x - r >= 0.5`
                // (exact, unlike `floor(x + 0.5)`), with the sign of `x` so
                // `[-0.5, -0]` rounds to `-0`; differs from wasm's banker's
                // `f64.nearest` (e.g. `round(2.5) == 3`, not `2`).
                function.instruction(&Instruction::F64Floor);
                x(function, scratch);
                function.instruction(&Instruction::F64Floor);
                function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
                function.instruction(&Instruction::F64Add);
                x(function, scratch);
                x(function, scratch);
                function.instruction(&Instruction::F64Floor);
                function.instruction(&Instruction::F64Sub);
                function.instruction(&Instruction::F64Const(Ieee64::from(0.5)));
                function.instruction(&Instruction::F64Lt);
                // `select(floor, floor + 1, frac < 0.5)`.
                function.instruction(&Instruction::Select);
                x(function, scratch);
                function.instruction(&Instruction::F64Copysign);
            } else {
                // `Sign`: `select(x, copysign(1, x), x == 0 || x != x)`
                // (keeps `±0` and NaN).
                function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
                x(function, scratch);
                function.instruction(&Instruction::F64Copysign);
                x(function, scratch);
                function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
                function.instruction(&Instruction::F64Eq);
                x(function, scratch);
                x(function, scratch);
                function.instruction(&Instruction::F64Ne);
                function.instruction(&Instruction::I32Or);
                function.instruction(&Instruction::Select);
            }
            emit_box_f64(function, helpers);
        }
    }
}

/// The host [`abi::METHOD_OP_MATH`] function code of a unary `Math` method
/// without an exact wasm instruction, or `None`.
fn host_math_code(op: MathOp) -> Option<i64> {
    Some(match op {
        MathOp::Cbrt => abi::MATH_FN_CBRT,
        MathOp::Log => abi::MATH_FN_LOG,
        MathOp::Log2 => abi::MATH_FN_LOG2,
        MathOp::Log10 => abi::MATH_FN_LOG10,
        MathOp::Log1p => abi::MATH_FN_LOG1P,
        MathOp::Exp => abi::MATH_FN_EXP,
        MathOp::Expm1 => abi::MATH_FN_EXPM1,
        MathOp::Sin => abi::MATH_FN_SIN,
        MathOp::Cos => abi::MATH_FN_COS,
        MathOp::Tan => abi::MATH_FN_TAN,
        MathOp::Asin => abi::MATH_FN_ASIN,
        MathOp::Acos => abi::MATH_FN_ACOS,
        MathOp::Atan => abi::MATH_FN_ATAN,
        MathOp::Sinh => abi::MATH_FN_SINH,
        MathOp::Cosh => abi::MATH_FN_COSH,
        MathOp::Tanh => abi::MATH_FN_TANH,
        MathOp::Asinh => abi::MATH_FN_ASINH,
        MathOp::Acosh => abi::MATH_FN_ACOSH,
        MathOp::Atanh => abi::MATH_FN_ATANH,
        _ => return None,
    })
}

/// Call the host `Math` function `code` on the number jsval on the stack
/// (and the one in i64 local `second`), leaving its number jsval.
fn emit_host_math(code: i64, second: Option<u32>, function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::I32Const(abi::METHOD_OP_MATH));
    match second {
        Some(local) => function.instruction(&Instruction::LocalGet(local)),
        None => function.instruction(&Instruction::I64Const(UNDEFINED_BITS)),
    };
    function.instruction(&Instruction::I64Const(code));
    function.instruction(&Instruction::Call(helpers.method));
}

/// The first of the i64 slots variadic `Math.min`/`Math.max` stash evaluated
/// arguments in before converting them: the `Date` field-method scratch
/// (`+24`..`+29`), idle while only `__to_number` runs.
const MIN_MAX_STASH: u32 = 24;

/// The number of arguments [`MIN_MAX_STASH`] holds.
const MIN_MAX_STASH_LEN: usize = 6;

/// Emit variadic `Math.min`/`Math.max` (see [`emit_math_call`]).
///
/// Every argument is evaluated before the first `ToNumber`. When no argument
/// after the first can observe or cause a side effect (literals and local
/// reads), each is converted right after its evaluation and folded at once.
/// Otherwise all are evaluated onto the stack and stashed, then converted and
/// folded left to right; more arguments than the stash holds go through the
/// spread fold over a fresh array.
fn emit_min_max(
    op: MathOp,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let fold = if matches!(op, MathOp::Min) {
        Instruction::F64Min
    } else {
        Instruction::F64Max
    };
    let identity = if matches!(op, MathOp::Min) {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };
    let interleaved = args.iter().skip(1).all(is_inert);
    if !interleaved && args.len() > MIN_MAX_STASH_LEN {
        let items = args.iter().cloned().map(SpreadElem::Item).collect();
        let spread = IrNode::ArraySpread(items);
        emit_math_spread(op, &spread, function, scratch, strings, helpers);
        return;
    }
    if !interleaved {
        for arg in args {
            emit_node(arg, function, scratch, strings, helpers);
        }
        for i in (0..args.len()).rev() {
            function.instruction(&Instruction::LocalSet(stash_slot(scratch, i)));
        }
    }
    function.instruction(&Instruction::F64Const(Ieee64::from(identity)));
    for (i, arg) in args.iter().enumerate() {
        let local = if interleaved {
            emit_node(arg, function, scratch, strings, helpers);
            if yields_number(arg) {
                function.instruction(&Instruction::F64ReinterpretI64);
                function.instruction(&fold);
                continue;
            }
            function.instruction(&Instruction::LocalSet(scratch));
            scratch
        } else {
            stash_slot(scratch, i)
        };
        if !yields_number(arg) {
            emit_coerce_number(function, local, helpers);
        }
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&fold);
    }
    emit_box_f64(function, helpers);
}

/// The stash slot of variadic `Math.min`/`Math.max` argument `i`.
fn stash_slot(scratch: u32, i: usize) -> u32 {
    scratch + MIN_MAX_STASH + u32::try_from(i).unwrap_or(0)
}

/// Whether evaluating `node` neither has a side effect nor reads state a
/// user `valueOf` could change: a primitive literal or a wasm-local read.
fn is_inert(node: &IrNode) -> bool {
    matches!(
        node,
        IrNode::Num(_)
            | IrNode::Str(_)
            | IrNode::Bool(_)
            | IrNode::Null
            | IrNode::Undefined
            | IrNode::Local(_)
    )
}

/// Whether `node` always evaluates to a number jsval, so `ToNumber` of it is
/// the identity.
fn yields_number(node: &IrNode) -> bool {
    match node {
        IrNode::Num(_)
        | IrNode::MathCall { .. }
        | IrNode::MathSpread { .. }
        | IrNode::ConvertCall {
            op: ConvertOp::ToNumber,
            ..
        } => true,
        IrNode::Binary { op, .. } => matches!(
            op,
            BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::UShr
        ),
        _ => false,
    }
}

/// Emit `Math.max(...)` / `Math.min(...)` / `Math.hypot(...)` with a spread
/// argument, leaving a number jsval on the stack. The `args` node (an
/// [`IrNode::ArraySpread`]) is materialized into a heap array; a counted loop
/// then folds `f64.max`/`f64.min` over its elements' `ToNumber` from the
/// identity infinity — matching the fixed-arity variadic path in
/// [`emit_math_call`], but over a runtime-length array. Folding has no side
/// effect, so converting each element as it is folded runs the conversions in
/// argument order, after every argument.
///
/// `hypot` folds a scaled sum of squares instead (`scale` the largest
/// magnitude so far in the accumulator, `ssq` the sum of `(|x| / scale)²`, so
/// no square overflows or underflows), noting an infinite and a NaN element
/// in flag bits; after the loop an infinity wins, then NaN, else the result is
/// `scale * sqrt(ssq)`.
pub(super) fn emit_math_spread(
    op: MathOp,
    args: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    // Reuse the array higher-order loop's scratch slots (idle here): the array
    // element store, its length, the loop index, and the i64 accumulator. The
    // `args` build finishes using these before the fold seeds them.
    let base = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let acc = scratch + 13;
    // The hypot `ssq` (f64 bits) and its infinity / NaN flags: the `Date`
    // field-method slots, idle while the fold runs.
    let (ssq, flags) = (scratch + 24, scratch + 25);
    let is_min = matches!(op, MathOp::Min);
    let is_hypot = matches!(op, MathOp::Hypot);

    // Materialize the (fresh, private) argument array; its length -> `len`, its
    // element store -> `base`, idx = 0.
    emit_node(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalSet(len));
    function.instruction(&Instruction::LocalGet(base));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalSet(base));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(idx));

    // acc = identity infinity (as f64 bits): +Infinity for min, -Infinity for max.
    let identity = if is_hypot {
        0.0
    } else if is_min {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };
    function.instruction(&Instruction::F64Const(Ieee64::from(identity)));
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(acc));
    if is_hypot {
        function.instruction(&Instruction::I64Const(0));
        function.instruction(&Instruction::LocalSet(ssq));
        function.instruction(&Instruction::I64Const(0));
        function.instruction(&Instruction::LocalSet(flags));
    }

    // Counted loop: `while (idx < len) { acc = op(acc, array[idx]); idx++ }`.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers);
    enter_frame(helpers);
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // acc = op(acc, ToNumber(array[idx])). The element sits in `scratch`,
    // idle once the array is built.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::LocalSet(scratch));
    emit_coerce_number(function, scratch, helpers);
    if is_hypot {
        emit_hypot_step(function, scratch, acc, ssq, flags);
    } else {
        function.instruction(&Instruction::LocalGet(acc));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::LocalGet(scratch));
        function.instruction(&Instruction::F64ReinterpretI64);
        if is_min {
            function.instruction(&Instruction::F64Min);
        } else {
            function.instruction(&Instruction::F64Max);
        }
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(acc));
    }
    // idx++ and continue.
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(idx));
    function.instruction(&Instruction::Br(0));
    leave_frame(helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // The accumulator holds the folded result's f64 bits; rebox it as a number.
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::F64ReinterpretI64);
    if is_hypot {
        // `flags & 1 ? Infinity : flags & 2 ? NaN : scale * sqrt(ssq)`.
        function.instruction(&Instruction::LocalGet(ssq));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Sqrt);
        function.instruction(&Instruction::F64Mul);
        function.instruction(&Instruction::F64Const(Ieee64::from(f64::NAN)));
        function.instruction(&Instruction::LocalGet(flags));
        function.instruction(&Instruction::I64Const(2));
        function.instruction(&Instruction::I64And);
        function.instruction(&Instruction::I64Eqz);
        function.instruction(&Instruction::Select);
        function.instruction(&Instruction::F64Const(Ieee64::from(f64::INFINITY)));
        function.instruction(&Instruction::LocalGet(flags));
        function.instruction(&Instruction::I64Const(1));
        function.instruction(&Instruction::I64And);
        function.instruction(&Instruction::I64Eqz);
        function.instruction(&Instruction::Select);
    }
    emit_box_f64(function, helpers);
}

/// One `Math.hypot` fold step over the number jsval in `x` (see
/// [`emit_math_spread`]): an infinite or NaN `|x|` sets flag bit 1 / 2; a
/// new largest `|x|` rescales `ssq` to `1 + ssq * (scale / |x|)²` and becomes
/// `scale`; any other nonzero `|x|` adds `(|x| / scale)²`.
fn emit_hypot_step(function: &mut Function, x: u32, scale: u32, ssq: u32, flags: u32) {
    let get = |function: &mut Function, local: u32| {
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
    };
    let set_flag = |function: &mut Function, bit: i64| {
        function.instruction(&Instruction::LocalGet(flags));
        function.instruction(&Instruction::I64Const(bit));
        function.instruction(&Instruction::I64Or);
        function.instruction(&Instruction::LocalSet(flags));
    };
    // `x = |x|`.
    get(function, x);
    function.instruction(&Instruction::F64Abs);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(x));
    get(function, x);
    function.instruction(&Instruction::F64Const(Ieee64::from(f64::INFINITY)));
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    set_flag(function, 1);
    function.instruction(&Instruction::Else);
    get(function, x);
    get(function, x);
    function.instruction(&Instruction::F64Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    set_flag(function, 2);
    function.instruction(&Instruction::Else);
    get(function, x);
    get(function, scale);
    function.instruction(&Instruction::F64Gt);
    function.instruction(&Instruction::If(BlockType::Empty));
    // ssq = 1 + ssq * (scale / x)²; scale = x.
    let ratio_squared = |function: &mut Function, num: u32, den: u32| {
        for _ in 0..2 {
            get(function, num);
            get(function, den);
            function.instruction(&Instruction::F64Div);
        }
        function.instruction(&Instruction::F64Mul);
    };
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    get(function, ssq);
    ratio_squared(function, scale, x);
    function.instruction(&Instruction::F64Mul);
    function.instruction(&Instruction::F64Add);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(ssq));
    function.instruction(&Instruction::LocalGet(x));
    function.instruction(&Instruction::LocalSet(scale));
    function.instruction(&Instruction::Else);
    // `0 < x <= scale`: ssq += (x / scale)².
    get(function, x);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Gt);
    function.instruction(&Instruction::If(BlockType::Empty));
    get(function, ssq);
    ratio_squared(function, x, scale);
    function.instruction(&Instruction::F64Add);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(ssq));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Emit `Math.pow(base, exp)` (and `base ** exp`), leaving a number jsval on the
/// stack.
///
/// wasm has no `f64` power op, so `base^|exp|` is built in two import-free stages:
/// the integer part `cnt = trunc(|exp|)` by square-and-multiply over a halving
/// counter (at most ~1024 rounds even for `|exp|` near `f64::MAX`, so huge
/// exponents such as `2 ** -2147483648` terminate and saturate exactly), and the
/// fractional part `f = |exp| - cnt` (`0 ≤ f < 1`) by the classic bit-by-bit
/// square-root method: successive `f64.sqrt` yields `base^(1/2^i)`, and each `1`
/// bit of `f`'s binary expansion multiplies it in (`2 ** 0.5` therefore reduces to
/// `sqrt(2)`). A negative exponent reciprocates the result; a negative finite base
/// with a fractional exponent yields `NaN` (via `sqrt` of a negative), matching JS.
///
/// The spec's `Number::exponentiate` special cases are resolved up front: a zero
/// exponent gives `1` (even for a `NaN` base), a `NaN` operand gives `NaN`, and so
/// does `±1` raised to `±Infinity`. An infinite exponent is clamped to `f64::MAX`
/// (an even integer), which saturates every other base correctly. Under a
/// non-integer exponent a `±0` / `±Infinity` base loses its sign (the exponent is
/// not an odd integer), so `(-0) ** 0.5` is `+0` and `(-Infinity) ** 0.5` is
/// `+Infinity`.
///
/// The base and exponent are each fully evaluated and stashed BEFORE the loop
/// touches any scratch local, so nested `Math` calls (which reuse the same scratch
/// locals) cannot clobber them — the same evaluate-then-store discipline as
/// [`emit_mod`].
#[allow(clippy::too_many_lines)]
pub(super) fn emit_pow(
    base: &IrNode,
    exp: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    /// Push the f64 held (as bits) in i64 local `local`.
    fn get(function: &mut Function, local: u32) {
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::F64ReinterpretI64);
    }
    /// Pop an f64 into i64 local `local` (as bits).
    fn set(function: &mut Function, local: u32) {
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(local));
    }
    /// `local = local * other`.
    fn mul_into(function: &mut Function, local: u32, other: u32) {
        get(function, local);
        get(function, other);
        function.instruction(&Instruction::F64Mul);
        set(function, local);
    }
    /// `res = base^cnt` for an integer `cnt` in `[0, f64::MAX]` by
    /// square-and-multiply over a halving counter (consumes `base` and `cnt`).
    fn int_pow(function: &mut Function, res: u32, base: u32, cnt: u32) {
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        set(function, res);
        // while cnt >= 1 { if cnt is odd { result *= base }; base *= base;
        // cnt = floor(cnt / 2) }
        function.instruction(&Instruction::Block(BlockType::Empty));
        function.instruction(&Instruction::Loop(BlockType::Empty));
        get(function, cnt);
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        function.instruction(&Instruction::F64Lt);
        function.instruction(&Instruction::BrIf(1));
        // odd: floor(cnt * 0.5) * 2 != cnt.
        get(function, cnt);
        function.instruction(&Instruction::F64Const(Ieee64::from(0.5)));
        function.instruction(&Instruction::F64Mul);
        function.instruction(&Instruction::F64Floor);
        function.instruction(&Instruction::F64Const(Ieee64::from(2.0)));
        function.instruction(&Instruction::F64Mul);
        get(function, cnt);
        function.instruction(&Instruction::F64Ne);
        function.instruction(&Instruction::If(BlockType::Empty));
        mul_into(function, res, base);
        function.instruction(&Instruction::End);
        mul_into(function, base, base);
        get(function, cnt);
        function.instruction(&Instruction::F64Const(Ieee64::from(0.5)));
        function.instruction(&Instruction::F64Mul);
        function.instruction(&Instruction::F64Floor);
        set(function, cnt);
        function.instruction(&Instruction::Br(0));
        function.instruction(&Instruction::End); // loop
        function.instruction(&Instruction::End); // block
    }
    let f64c = |function: &mut Function, v: f64| {
        function.instruction(&Instruction::F64Const(Ieee64::from(v)));
    };

    // i64 scratch locals hold f64 bit patterns (reinterpreted on read); the i32
    // scratch at `scratch + 3` (unused during pow, which never allocates) holds the
    // negative-exponent flag. `scratch + 4` (the short-circuit-operand temp, dead
    // on any path that evaluates a `**` sub-expression) holds the fractional part,
    // and `scratch + 24` / `+25` (`Date` field-method slots; no child
    // sub-expression is live while pow runs) keep the original base and integer
    // count for the fractional stage and the underflow fallback.
    let s_base = scratch;
    let s_res = scratch + 1;
    let s_cnt = scratch + 2;
    let s_neg = scratch + 3;
    let s_frac = scratch + 4;
    let s_orig = scratch + 24;
    let s_cnt0 = scratch + 25;

    // Evaluate both operands onto the stack FIRST, then stash (exp is on top), so a
    // nested `**`/`Math.pow` inside `exp` finishes before any scratch local is set
    // and cannot clobber the stashed base — the same discipline as [`emit_mod`].
    emit_node(base, function, scratch, strings, helpers);
    emit_node(exp, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s_cnt));
    function.instruction(&Instruction::LocalSet(s_base));
    emit_coerce_number(function, s_base, helpers);
    emit_coerce_number(function, s_cnt, helpers);

    // The whole computation leaves its answer in `res`; special cases branch out.
    function.instruction(&Instruction::Block(BlockType::Empty));

    // exp == ±0 → 1.
    f64c(function, 1.0);
    set(function, s_res);
    get(function, s_cnt);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::BrIf(0));

    // exp or base NaN, or |base| == 1 with |exp| == Infinity → NaN.
    f64c(function, f64::NAN);
    set(function, s_res);
    get(function, s_cnt);
    get(function, s_cnt);
    function.instruction(&Instruction::F64Ne);
    get(function, s_base);
    get(function, s_base);
    function.instruction(&Instruction::F64Ne);
    function.instruction(&Instruction::I32Or);
    get(function, s_cnt);
    function.instruction(&Instruction::F64Abs);
    f64c(function, f64::INFINITY);
    function.instruction(&Instruction::F64Eq);
    get(function, s_base);
    function.instruction(&Instruction::F64Abs);
    f64c(function, 1.0);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::BrIf(0));

    // neg = exp < 0.
    get(function, s_cnt);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::LocalSet(s_neg));

    // aexp = abs(exp), then split into frac = aexp - trunc(aexp) (0 ≤ frac < 1;
    // NaN for an infinite exponent, which skips the fractional stage) and
    // cnt = min(trunc(aexp), f64::MAX) (an infinite exponent clamps to the
    // largest finite even integer).
    get(function, s_cnt);
    function.instruction(&Instruction::F64Abs);
    set(function, s_cnt);
    get(function, s_cnt);
    get(function, s_cnt);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Sub);
    set(function, s_frac);
    get(function, s_cnt);
    function.instruction(&Instruction::F64Trunc);
    f64c(function, f64::MAX);
    function.instruction(&Instruction::F64Min);
    set(function, s_cnt);

    // A non-integer exponent is not an odd integer, so a ±0 / ±Infinity base
    // yields a positive result: drop its sign.
    get(function, s_frac);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Gt);
    get(function, s_base);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Eq);
    get(function, s_base);
    function.instruction(&Instruction::F64Abs);
    f64c(function, f64::INFINITY);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Empty));
    get(function, s_base);
    function.instruction(&Instruction::F64Abs);
    set(function, s_base);
    function.instruction(&Instruction::End);

    // orig = base (the squaring loop below consumes `base`).
    function.instruction(&Instruction::LocalGet(s_base));
    function.instruction(&Instruction::LocalSet(s_orig));

    // Keep the integer count for the underflow fallback below.
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::LocalSet(s_cnt0));
    int_pow(function, s_res, s_base, s_cnt);

    // A negative integer exponent whose positive power overflowed (e.g.
    // `2 ** -1074`, where `2^1074` is Infinity) would reciprocate to 0; rebuild it
    // as `(1 / orig)^cnt` instead, which reaches the subnormal range, and skip the
    // reciprocal.
    function.instruction(&Instruction::LocalGet(s_neg));
    get(function, s_frac);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);
    get(function, s_res);
    function.instruction(&Instruction::F64Abs);
    f64c(function, f64::INFINITY);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);
    get(function, s_orig);
    function.instruction(&Instruction::F64Abs);
    f64c(function, f64::INFINITY);
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Empty));
    f64c(function, 1.0);
    get(function, s_orig);
    function.instruction(&Instruction::F64Div);
    set(function, s_base);
    function.instruction(&Instruction::LocalGet(s_cnt0));
    function.instruction(&Instruction::LocalSet(s_cnt));
    int_pow(function, s_res, s_base, s_cnt);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(s_neg));
    function.instruction(&Instruction::End);

    // Fractional part: if frac > 0, apply orig^frac by the bit-by-bit sqrt method.
    // `mul` (reusing s_base) starts at sqrt(orig) = orig^(1/2) and is square-rooted
    // each round to orig^(1/2^i); each round doubles frac and, when it reaches ≥ 1,
    // multiplies `mul` into result and drops the consumed bit. 52 rounds exhaust the
    // f64 mantissa; dyadic fractions (e.g. 0.5) drive frac to 0 and finish early.
    get(function, s_frac);
    f64c(function, 0.0);
    function.instruction(&Instruction::F64Gt);
    function.instruction(&Instruction::If(BlockType::Empty));
    get(function, s_orig);
    function.instruction(&Instruction::F64Sqrt);
    set(function, s_base);
    for _ in 0..52 {
        // frac *= 2.
        get(function, s_frac);
        f64c(function, 2.0);
        function.instruction(&Instruction::F64Mul);
        set(function, s_frac);
        // if frac >= 1 { result *= mul; frac -= 1 }
        get(function, s_frac);
        f64c(function, 1.0);
        function.instruction(&Instruction::F64Ge);
        function.instruction(&Instruction::If(BlockType::Empty));
        mul_into(function, s_res, s_base);
        get(function, s_frac);
        f64c(function, 1.0);
        function.instruction(&Instruction::F64Sub);
        set(function, s_frac);
        function.instruction(&Instruction::End);
        // mul = sqrt(mul).
        get(function, s_base);
        function.instruction(&Instruction::F64Sqrt);
        set(function, s_base);
    }
    function.instruction(&Instruction::End); // if frac > 0

    // if neg { result = 1.0 / result }.
    function.instruction(&Instruction::LocalGet(s_neg));
    function.instruction(&Instruction::If(BlockType::Empty));
    f64c(function, 1.0);
    get(function, s_res);
    function.instruction(&Instruction::F64Div);
    set(function, s_res);
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::End); // result block

    get(function, s_res);
    emit_box_f64(function, helpers);
}

/// Emit a conversion built-in call, leaving its result jsval on the stack.
///
/// `Boolean(x)` is emitted inline: JS truthiness (via [`emit_cond`]) selects
/// between the boxed `true`/`false` jsvals, no host call. `Number(x)` is the
/// shared `ToNumber`, `String(x)` the guest `__to_string`; `parseInt` /
/// `parseFloat` convert their argument with `__to_string` and parse it on the
/// host (`__parse_int` / the `__convert` parse-float op).
pub(super) fn emit_convert_call(
    op: ConvertOp,
    arg: &IrNode,
    radix: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if matches!(op, ConvertOp::ToBoolean) {
        // Box the JS truthiness of the argument as a boolean jsval:
        // `truthy ? TRUE_BITS : FALSE_BITS`.
        function.instruction(&Instruction::I64Const(TRUE_BITS));
        function.instruction(&Instruction::I64Const(FALSE_BITS));
        emit_cond(arg, function, scratch, strings, helpers);
        function.instruction(&Instruction::Select);
        return;
    }
    if matches!(op, ConvertOp::ToNumber) {
        // The shared ToNumber: a number passes through, anything else is
        // converted by `__to_number`.
        emit_node(arg, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(scratch));
        emit_coerce_number(function, scratch, helpers);
        function.instruction(&Instruction::LocalGet(scratch));
        return;
    }
    if matches!(op, ConvertOp::ParseInt) {
        // `__parse_int(ToString(s), radix)`: an omitted radix is passed as
        // `undefined`. Both operands are evaluated before the conversion.
        emit_node(arg, function, scratch, strings, helpers);
        match radix {
            Some(radix) => emit_node(radix, function, scratch, strings, helpers),
            None => {
                function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            }
        }
        function.instruction(&Instruction::LocalSet(scratch + 1));
        function.instruction(&Instruction::LocalSet(scratch));
        emit_to_string_jsval(function, scratch, helpers);
        function.instruction(&Instruction::LocalGet(scratch + 1));
        function.instruction(&Instruction::Call(helpers.parse_int));
        return;
    }
    // `String(x)` is `ToString` in the guest; `parseFloat` parses the
    // argument's `ToString` on the host.
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));
    if matches!(op, ConvertOp::String) {
        emit_symbol_descriptive_string(function, scratch, strings, helpers);
    }
    emit_to_string_jsval(function, scratch, helpers);
    if matches!(op, ConvertOp::ParseFloat) {
        function.instruction(&Instruction::I32Const(CONVERT_OP_PARSE_FLOAT));
        function.instruction(&Instruction::Call(helpers.convert));
    }
}

/// Replace a symbol in i64 local `local` with its `SymbolDescriptiveString`:
/// `"Symbol(" + description + ")"`, an `undefined` description as `""`. The
/// i64 local `local + 1` is clobbered.
fn emit_symbol_descriptive_string(
    function: &mut Function,
    local: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let desc = local + 1;
    emit_is_symbol(function, local);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(static_key(strings, SYMBOL_OPEN)));
    function.instruction(&Instruction::I32Const(static_key(strings, "")));
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: SYMBOL_DESCRIPTION_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalTee(desc));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(desc));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::I32Const(static_key(strings, SYMBOL_CLOSE)));
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(local));
    function.instruction(&Instruction::End);
}

/// Push `ToString` of the jsval in `local` as a heap-string jsval (via the
/// guest `__to_string`), propagating a throw from a user `toString`/`valueOf`.
fn emit_to_string_jsval(function: &mut Function, local: u32, helpers: Helpers<'_>) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::Call(helpers.to_str()));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    emit_propagate_if_pending(function, helpers);
}

/// Emit the array fast path `IrNode::MethodCall`, leaving its result jsval
/// on the stack: the receiver, then (`indexOf`/`includes`) the `METHOD_OP_*`
/// flag, the argument (`undefined` when missing) and an unused `undefined`
/// slot, then a `call` of the host `__method` import. `join` runs in the guest
/// (`__array_join`) so an element's `ToString` can call a user `toString`
/// (which may throw).
pub(super) fn emit_method_call(
    op: MethodOp,
    recv: &IrNode,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(recv, function, scratch, strings, helpers);
    let flag = match op {
        MethodOp::Join => None,
        MethodOp::IndexOf => Some(METHOD_OP_INDEX_OF),
        MethodOp::Includes => Some(METHOD_OP_INCLUDES),
    };
    if let Some(flag) = flag {
        function.instruction(&Instruction::I32Const(flag));
    }
    match args.first() {
        Some(arg) => emit_node(arg, function, scratch, strings, helpers),
        None => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    }
    if flag.is_some() {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::Call(helpers.method));
    } else {
        function.instruction(&Instruction::Call(helpers.array_join()));
        emit_propagate_if_pending(function, helpers);
    }
}

/// Emit the `+` operator, a runtime type dispatch following `ToPrimitive`: if
/// EITHER operand is a heap value the shared `__add` helper converts both
/// (hint default; a user `valueOf`/`toString` may throw, propagated here) and
/// concatenates or adds. Only when both operands are non-heap primitives
/// (number/boolean/null/undefined) does it apply `ToNumber` to both and add
/// inline.
fn emit_add(
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s0 = scratch;
    let s1 = scratch + 1;
    emit_node(lhs, function, scratch, strings, helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));

    // either_heap = is_pointer(s0) | is_pointer(s1).
    emit_is_pointer(function, s0);
    emit_is_pointer(function, s1);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::Call(helpers.add_slow()));
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::Else);
    // ELSE: numeric add — ToNumber both operands (`null` -> 0, booleans ->
    // 0/1, `undefined` -> NaN), unbox to f64, add, rebox.
    emit_coerce_number(function, s0, helpers);
    emit_coerce_number(function, s1, helpers);
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Add);
    emit_box_f64(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

pub(super) fn emit_binary(
    op: BinOp,
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        // `+` is a runtime type dispatch (string concat vs numeric add); see
        // [`emit_add`].
        BinOp::Add => emit_add(lhs, rhs, function, scratch, strings, helpers),
        // Subtraction/multiplication/division: see [`emit_arith`].
        BinOp::Sub | BinOp::Mul | BinOp::Div => {
            emit_arith(op, lhs, rhs, function, scratch, strings, helpers);
        }
        // Remainder: wasm has no `f64.rem`; delegate to the dedicated emitter.
        BinOp::Mod => emit_mod(lhs, rhs, function, scratch, strings, helpers),
        // Bitwise/shift: see [`emit_bitwise`].
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr | BinOp::UShr => {
            emit_bitwise(op, lhs, rhs, function, scratch, strings, helpers);
        }
        // Relational: a runtime type dispatch. Two heap strings compare
        // lexicographically by bytes (`__str_cmp`); two numbers compare as
        // `f64`s; anything else goes to `__compare` (ToPrimitive with hint
        // number, whose user `valueOf`/`toString` may throw, then the same
        // dispatch). Each path yields an i32 wasm bool that is boxed as a
        // boolean jsval (TRUE/FALSE) via `select`.
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let s0 = scratch;
            let s1 = scratch + 1;
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            emit_node(lhs, function, scratch, strings, helpers);
            emit_node(rhs, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(s1));
            function.instruction(&Instruction::LocalSet(s0));

            // both_strings = is_string(s0) & is_string(s1).
            emit_is_string(function, s0);
            emit_is_string(function, s1);
            function.instruction(&Instruction::I32And);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            enter_frame(helpers);
            // THEN: c = __str_cmp(s0, s1); map `c <op> 0` (signed, c ∈ -1/0/1).
            function.instruction(&Instruction::LocalGet(s0));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::LocalGet(s1));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::Call(helpers.str_cmp));
            function.instruction(&Instruction::I32Const(0));
            let (str_cmp, num_cmp, rel) = match op {
                BinOp::Lt => (Instruction::I32LtS, Instruction::F64Lt, REL_LT),
                BinOp::Gt => (Instruction::I32GtS, Instruction::F64Gt, REL_GT),
                BinOp::Le => (Instruction::I32LeS, Instruction::F64Le, REL_LE),
                BinOp::Ge => (Instruction::I32GeS, Instruction::F64Ge, REL_GE),
                _ => unreachable!("non-relational ops handled elsewhere"),
            };
            function.instruction(&str_cmp);
            function.instruction(&Instruction::Else);
            // ELSE: two numbers compare inline; anything else via `__compare`.
            emit_is_number(function, s0);
            emit_is_number(function, s1);
            function.instruction(&Instruction::I32And);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            enter_frame(helpers);
            function.instruction(&Instruction::LocalGet(s0));
            function.instruction(&Instruction::F64ReinterpretI64);
            function.instruction(&Instruction::LocalGet(s1));
            function.instruction(&Instruction::F64ReinterpretI64);
            function.instruction(&num_cmp);
            function.instruction(&Instruction::Else);
            function.instruction(&Instruction::LocalGet(s0));
            function.instruction(&Instruction::LocalGet(s1));
            function.instruction(&Instruction::I32Const(rel));
            function.instruction(&Instruction::Call(helpers.compare()));
            emit_propagate_if_pending(function, helpers);
            leave_frame(helpers);
            function.instruction(&Instruction::End);
            leave_frame(helpers);
            function.instruction(&Instruction::End);
            function.instruction(&Instruction::Select);
        }
        // Strict equality: type-then-value, yielding a boolean jsval.
        BinOp::Eq | BinOp::Ne => {
            emit_strict_eq(
                lhs,
                rhs,
                matches!(op, BinOp::Ne),
                function,
                scratch,
                strings,
                helpers,
            );
        }
        // Loose equality (`==`/`!=`): the runtime `__loose_eq` kernel does the
        // Abstract-Equality coercion (an object operand's `ToPrimitive` may
        // throw, propagated here); box its i32 result (negated for `!=`) as a
        // boolean jsval via `select` over TRUE/FALSE.
        BinOp::LooseEq | BinOp::LooseNe => {
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            emit_node(lhs, function, scratch, strings, helpers);
            emit_node(rhs, function, scratch, strings, helpers);
            function.instruction(&Instruction::Call(helpers.loose_eq));
            emit_propagate_if_pending(function, helpers);
            if matches!(op, BinOp::LooseNe) {
                function.instruction(&Instruction::I32Eqz);
            }
            function.instruction(&Instruction::Select);
        }
    }
}

/// Emit strict `===` (or `!==` when `negate`) over two jsval operands, leaving a
/// boolean jsval (TRUE/FALSE) on the stack. No control flow is used.
///
/// Both operands are evaluated onto the stack first, then stashed in the two
/// scratch locals (`scratch`/`scratch + 1`) so their bits can be inspected
/// twice. If both operands are numbers (per the NaN-box layout) they compare as
/// `f64` (so `NaN === NaN` is false and `+0 === -0` is true); otherwise they
/// compare by identity — the raw `i64` bits are equal iff the values are (which
/// makes different types, differing in tag, always unequal).
pub(super) fn emit_strict_eq(
    lhs: &IrNode,
    rhs: &IrNode,
    negate: bool,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s0 = scratch;
    let s1 = scratch + 1;
    // Evaluate both operands to the stack, then stash (rhs is on top).
    emit_node(lhs, function, scratch, strings, helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s1));
    function.instruction(&Instruction::LocalSet(s0));

    // Boolean-boxing operands for the final `select`.
    function.instruction(&Instruction::I64Const(TRUE_BITS));
    function.instruction(&Instruction::I64Const(FALSE_BITS));

    // eq is a 3-way runtime dispatch on the two stashed operands:
    //   both strings -> __str_eq (byte content);
    //   else both numbers -> f64 equality (so NaN !== NaN, +0 === -0);
    //   else identity — raw i64 bits equal (different types differ in tag, so
    //   they compare unequal). The string path is guarded by an `if` (not a
    //   branchless `select`) so `__str_eq` never runs on a non-pointer whose
    //   wrapped bits would be a huge, invalid byte offset. Since every heap kind
    //   shares tag 0b101, "both strings" reads each operand's TYPE BYTE; two
    //   arrays/objects fall to the identity (i64 bits) compare — reference
    //   equality, matching JS (`[1] === [1]` is `false`).
    emit_is_string(function, s0);
    emit_is_string(function, s1);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    // THEN: byte-content equality of the two heap strings.
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.str_eq));
    function.instruction(&Instruction::Else);
    // ELSE: both_numbers ? f64_eq(s0, s1) : bits_eq(s0, s1).
    // val1 = numeric equality (f64 compare).
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    // val2 = identity equality (raw i64 bits compare).
    function.instruction(&Instruction::LocalGet(s0));
    function.instruction(&Instruction::LocalGet(s1));
    function.instruction(&Instruction::I64Eq);
    // cond = is_number(s0) & is_number(s1).
    emit_is_number(function, s0);
    emit_is_number(function, s1);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::End);

    // `!==` is the negation of `===`.
    if negate {
        function.instruction(&Instruction::I32Eqz);
    }
    // Box the resulting i1 as TRUE/FALSE.
    function.instruction(&Instruction::Select);
}
