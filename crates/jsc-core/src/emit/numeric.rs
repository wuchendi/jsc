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
    strings: &HashMap<String, i32>,
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

/// Emit a `Math.*` built-in call, leaving its number-jsval result on the stack.
///
/// The unary ops unbox their single argument to `f64`, apply a native wasm op, and
/// rebox. `Round` uses JS semantics `floor(x + 0.5)`. `Min`/`Max` fold the native
/// `f64.min`/`f64.max` (which propagate NaN and order `-0`/`+0` like JS) over the
/// arguments starting from the identity (`+Infinity` for min, `-Infinity` for
/// max), so the empty-args case yields the correct infinity. `Pow` delegates to
/// [`emit_pow`].
pub(super) fn emit_math_call(
    op: MathOp,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match op {
        // Unary native-op methods: unbox → op → rebox.
        MathOp::Floor
        | MathOp::Ceil
        | MathOp::Trunc
        | MathOp::Abs
        | MathOp::Sqrt
        | MathOp::Round => {
            emit_node(&args[0], function, scratch, strings, helpers);
            function.instruction(&Instruction::F64ReinterpretI64);
            match op {
                MathOp::Floor => function.instruction(&Instruction::F64Floor),
                MathOp::Ceil => function.instruction(&Instruction::F64Ceil),
                MathOp::Trunc => function.instruction(&Instruction::F64Trunc),
                MathOp::Abs => function.instruction(&Instruction::F64Abs),
                MathOp::Sqrt => function.instruction(&Instruction::F64Sqrt),
                // JS `round(x)` = `floor(x + 0.5)`, differing from wasm's
                // banker's `f64.nearest` (e.g. `round(2.5) == 3`, not `2`).
                MathOp::Round => {
                    function.instruction(&Instruction::F64Const(Ieee64::from(0.5)));
                    function.instruction(&Instruction::F64Add);
                    function.instruction(&Instruction::F64Floor)
                }
                _ => unreachable!("outer match restricts op to the unary methods"),
            };
            emit_box_f64(function, helpers);
        }
        // Variadic min/max: seed with the identity infinity, then fold the native
        // pairwise op over each argument. `f64.min`/`f64.max` already match JS on
        // NaN (propagates) and signed zero.
        MathOp::Min | MathOp::Max => {
            let identity = if matches!(op, MathOp::Min) {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            };
            function.instruction(&Instruction::F64Const(Ieee64::from(identity)));
            for arg in args {
                emit_node(arg, function, scratch, strings, helpers);
                function.instruction(&Instruction::F64ReinterpretI64);
                if matches!(op, MathOp::Min) {
                    function.instruction(&Instruction::F64Min);
                } else {
                    function.instruction(&Instruction::F64Max);
                }
            }
            emit_box_f64(function, helpers);
        }
        MathOp::Pow => emit_pow(&args[0], &args[1], function, scratch, strings, helpers),
    }
}

/// Emit `Math.max(...)` / `Math.min(...)` with a spread argument, leaving
/// a number jsval on the stack. The `args` node (an [`IrNode::ArraySpread`]) is
/// materialized into a heap array; a counted loop then folds `f64.max`/`f64.min`
/// over its elements from the identity infinity — matching the fixed-arity
/// variadic path in [`emit_math_call`], but over a runtime-length array.
pub(super) fn emit_math_spread(
    op: MathOp,
    args: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // Reuse the array higher-order loop's scratch slots (idle here): the array
    // element store, its length, the loop index, and the i64 accumulator. The
    // `args` build finishes using these before the fold seeds them.
    let base = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let acc = scratch + 13;
    let is_min = matches!(op, MathOp::Min);

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
    let identity = if is_min {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };
    function.instruction(&Instruction::F64Const(Ieee64::from(identity)));
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(acc));

    // Counted loop: `while (idx < len) { acc = op(acc, array[idx]); idx++ }`.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // acc = op(acc, array[idx]).
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::F64ReinterpretI64);
    if is_min {
        function.instruction(&Instruction::F64Min);
    } else {
        function.instruction(&Instruction::F64Max);
    }
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(acc));
    // idx++ and continue.
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(idx));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // The accumulator holds the folded result's f64 bits; rebox it as a number.
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::F64ReinterpretI64);
    emit_box_f64(function, helpers);
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
    emit_to_string_jsval(function, scratch, helpers);
    if matches!(op, ConvertOp::ParseFloat) {
        function.instruction(&Instruction::I32Const(CONVERT_OP_PARSE_FLOAT));
        function.instruction(&Instruction::Call(helpers.convert));
    }
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match op {
        // `+` is a runtime type dispatch (string concat vs numeric add); see
        // [`emit_add`].
        BinOp::Add => emit_add(lhs, rhs, function, scratch, strings, helpers),
        // Subtraction/multiplication/division: stash both operands, coerce
        // each with ToNumber, unbox to f64, compute, rebox as a number.
        BinOp::Sub | BinOp::Mul | BinOp::Div => {
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
        // Remainder: wasm has no `f64.rem`; delegate to the dedicated emitter.
        BinOp::Mod => emit_mod(lhs, rhs, function, scratch, strings, helpers),
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
    strings: &HashMap<String, i32>,
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
