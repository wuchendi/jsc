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
    // base, its length, the loop index, and the i64 accumulator. The `args` build
    // finishes using these before the fold seeds them.
    let base = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let acc = scratch + 13;
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let is_min = matches!(op, MathOp::Min);

    // Materialize the argument array; base -> `base`, length -> `len`, idx = 0.
    emit_node(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));
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
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
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

/// Emit `Math.pow(base, exp)`, leaving a number jsval on the stack.
///
/// wasm has no `f64` power op, so `base^|exp|` is built in two import-free stages:
/// the integer part multiplies `base` into an accumulator once per unit of
/// `trunc(|exp|)` (exact for integer exponents, e.g. `pow(2, 10)`),
/// and the fractional part `f = |exp| - trunc(|exp|)` (`0 ≤ f < 1`) is applied by
/// the classic bit-by-bit square-root method: successive `f64.sqrt` yields
/// `base^(1/2^i)`, and each `1` bit of `f`'s binary expansion multiplies it in
/// (`2 ** 0.5` therefore reduces to `sqrt(2)`). A negative exponent reciprocates
/// the result; a negative base with a fractional exponent yields `NaN` (via
/// `sqrt` of a negative), matching JS.
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
    // i64 scratch locals hold f64 bit patterns (reinterpreted on read); the i32
    // scratch at `scratch + 3` (unused during pow, which never allocates) holds the
    // negative-exponent flag. `scratch + 4` (the short-circuit-operand temp, dead
    // on any path that evaluates a `**` sub-expression) holds the fractional part.
    let s_base = scratch;
    let s_res = scratch + 1;
    let s_cnt = scratch + 2;
    let s_neg = scratch + 3;
    let s_frac = scratch + 4;

    // Evaluate both operands onto the stack FIRST, then stash (exp is on top), so a
    // nested `**`/`Math.pow` inside `exp` finishes before any scratch local is set
    // and cannot clobber the stashed base — the same discipline as [`emit_mod`].
    emit_node(base, function, scratch, strings, helpers);
    emit_node(exp, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(s_cnt));
    function.instruction(&Instruction::LocalSet(s_base));
    emit_coerce_number(function, s_base, helpers);
    emit_coerce_number(function, s_cnt, helpers);

    // neg = exp < 0.
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::LocalSet(s_neg));

    // aexp = abs(exp), then split into cnt = trunc(aexp) (integer iteration count)
    // and frac = aexp - cnt (the fractional part, 0 ≤ frac < 1).
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Abs);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_cnt));
    // frac = aexp - trunc(aexp).
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::F64Sub);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_frac));
    // cnt = trunc(aexp).
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Trunc);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_cnt));

    // result = 1.0.
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_res));

    // while cnt >= 1 { result *= base; cnt -= 1 }
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if cnt < 1 break (out of the enclosing block).
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    function.instruction(&Instruction::F64Lt);
    function.instruction(&Instruction::BrIf(1));
    // result = result * base.
    function.instruction(&Instruction::LocalGet(s_res));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(s_base));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Mul);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_res));
    // cnt = cnt - 1.
    function.instruction(&Instruction::LocalGet(s_cnt));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    function.instruction(&Instruction::F64Sub);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_cnt));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // Fractional part: if frac > 0, apply base^frac by the bit-by-bit sqrt method.
    // `mul` (reusing s_base) starts at sqrt(base) = base^(1/2) and is squared-rooted
    // each round to base^(1/2^i); each round doubles frac and, when it reaches ≥ 1,
    // multiplies `mul` into result and drops the consumed bit. 52 rounds exhaust the
    // f64 mantissa; dyadic fractions (e.g. 0.5) drive frac to 0 and finish early.
    function.instruction(&Instruction::LocalGet(s_frac));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Gt);
    function.instruction(&Instruction::If(BlockType::Empty));
    // mul = sqrt(base).
    function.instruction(&Instruction::LocalGet(s_base));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Sqrt);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_base));
    for _ in 0..52 {
        // frac *= 2.
        function.instruction(&Instruction::LocalGet(s_frac));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Const(Ieee64::from(2.0)));
        function.instruction(&Instruction::F64Mul);
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(s_frac));
        // if frac >= 1 { result *= mul; frac -= 1 }
        function.instruction(&Instruction::LocalGet(s_frac));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        function.instruction(&Instruction::F64Ge);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::LocalGet(s_res));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::LocalGet(s_base));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Mul);
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(s_res));
        function.instruction(&Instruction::LocalGet(s_frac));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
        function.instruction(&Instruction::F64Sub);
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(s_frac));
        function.instruction(&Instruction::End);
        // mul = sqrt(mul).
        function.instruction(&Instruction::LocalGet(s_base));
        function.instruction(&Instruction::F64ReinterpretI64);
        function.instruction(&Instruction::F64Sqrt);
        function.instruction(&Instruction::I64ReinterpretF64);
        function.instruction(&Instruction::LocalSet(s_base));
    }
    function.instruction(&Instruction::End); // if frac > 0

    // if neg { result = 1.0 / result }.
    function.instruction(&Instruction::LocalGet(s_neg));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    function.instruction(&Instruction::LocalGet(s_res));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Div);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::LocalSet(s_res));
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::LocalGet(s_res));
    function.instruction(&Instruction::F64ReinterpretI64);
    emit_box_f64(function, helpers);
}

/// Emit a conversion built-in call, leaving its result jsval on the stack.
///
/// `Boolean(x)` is emitted inline: JS truthiness (via [`emit_cond`]) selects
/// between the boxed `true`/`false` jsvals, no host call. The other four delegate
/// to the host `__convert` import — the argument jsval and a `CONVERT_OP_*` flag
/// are pushed, and the import returns the converted jsval (a fresh heap string for
/// `String`, a number for the rest), reusing the run stage's `format_value`/parse
/// helpers so the number forms match `format_number`.
pub(super) fn emit_convert_call(
    op: ConvertOp,
    arg: &IrNode,
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
    let flag = match op {
        ConvertOp::ToString => CONVERT_OP_TO_STRING,
        ConvertOp::ParseInt => CONVERT_OP_PARSE_INT,
        ConvertOp::ParseFloat => CONVERT_OP_PARSE_FLOAT,
        ConvertOp::ToBoolean | ConvertOp::ToNumber => {
            unreachable!("ToBoolean/ToNumber handled above")
        }
    };
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32Const(flag));
    function.instruction(&Instruction::Call(helpers.convert));
}

/// Emit a non-callback array/string method call `IrNode::MethodCall`,
/// leaving its result jsval on the stack. The receiver is pushed, then the
/// `METHOD_OP_*` flag, then the two fixed argument slots (each the lowered
/// argument, or `undefined` when the method takes fewer), then a `call` of the
/// host `__method` import — which performs the whole operation host-side,
/// branching on the receiver's runtime TYPE BYTE for the overloaded
/// `indexOf`/`slice` and allocating any result string/array in guest memory.
pub(super) fn emit_method_call(
    op: MethodOp,
    recv: &IrNode,
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let flag = match op {
        MethodOp::IndexOf => METHOD_OP_INDEX_OF,
        MethodOp::Includes => METHOD_OP_INCLUDES,
        MethodOp::Join => METHOD_OP_JOIN,
        MethodOp::Slice => METHOD_OP_SLICE,
        MethodOp::ToUpper => METHOD_OP_TO_UPPER,
        MethodOp::ToLower => METHOD_OP_TO_LOWER,
        MethodOp::CharAt => METHOD_OP_CHAR_AT,
        MethodOp::Trim => METHOD_OP_TRIM,
        MethodOp::TrimStart => METHOD_OP_TRIM_START,
        MethodOp::TrimEnd => METHOD_OP_TRIM_END,
        MethodOp::Repeat => METHOD_OP_REPEAT,
        MethodOp::StartsWith => METHOD_OP_STARTS_WITH,
        MethodOp::EndsWith => METHOD_OP_ENDS_WITH,
        MethodOp::PadStart => METHOD_OP_PAD_START,
        MethodOp::PadEnd => METHOD_OP_PAD_END,
        MethodOp::Substring => METHOD_OP_SUBSTRING,
        MethodOp::Split => METHOD_OP_SPLIT,
    };
    // Receiver, then the op flag, then the two fixed argument slots (arg0, arg1)
    // in ABI order; a missing slot is `undefined`.
    emit_node(recv, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32Const(flag));
    for slot in 0..2 {
        match args.get(slot) {
            Some(arg) => emit_node(arg, function, scratch, strings, helpers),
            None => {
                function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            }
        }
    }
    function.instruction(&Instruction::Call(helpers.method));
}

/// Push the `ToString(ToPrimitive(v))` heap-string base (i32) of the `+`
/// operand in `local`. An array's primitive is its `join()` string (via the host
/// `__method` join op); every other value goes through the host `__convert`
/// to-string op, which returns a string unchanged and stringifies the rest.
fn emit_add_operand_to_string(function: &mut Function, local: u32, helpers: Helpers<'_>) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32Const(METHOD_OP_JOIN));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Call(helpers.method));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32WrapI64);
}

/// Emit the `+` operator, a runtime type dispatch following `ToPrimitive`: every
/// heap value (string, array, object, function, Date, Map/Set, promise, error)
/// has a string primitive, so if EITHER operand is a heap pointer both sides are
/// converted to strings ([`emit_add_operand_to_string`]) and concatenated into a
/// freshly allocated heap string. Only when both operands are non-heap
/// primitives (number/boolean/null/undefined) does it apply `ToNumber` to both
/// and add. User `valueOf`/`toString`/`Symbol.toPrimitive` overrides are not
/// consulted.
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
    // THEN: stringify both operands, concat, and rebox the result base offset as
    // a heap jsval.
    emit_add_operand_to_string(function, s0, helpers);
    emit_add_operand_to_string(function, s1, helpers);
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
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
        // lexicographically by bytes (`__str_cmp`); anything else coerces each
        // operand with ToNumber and compares the `f64`s. Either path yields an i32
        // wasm bool that is boxed as a boolean jsval (TRUE/FALSE) via `select`.
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let s0 = scratch;
            let s1 = scratch + 1;
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            emit_node(lhs, function, scratch, strings, helpers);
            emit_node(rhs, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(s1));
            function.instruction(&Instruction::LocalSet(s0));

            // both_strings = is_string(s0) & is_string(s1) — reads each operand's
            // TYPE BYTE (gated by is_pointer) so only two heap strings take the
            // lexicographic path; arrays/objects fall to the numeric compare.
            emit_is_string(function, s0);
            emit_is_string(function, s1);
            function.instruction(&Instruction::I32And);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            // THEN: c = __str_cmp(s0, s1); map `c <op> 0` (signed, c ∈ -1/0/1).
            function.instruction(&Instruction::LocalGet(s0));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::LocalGet(s1));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::Call(helpers.str_cmp));
            function.instruction(&Instruction::I32Const(0));
            let str_cmp = match op {
                BinOp::Lt => Instruction::I32LtS,
                BinOp::Gt => Instruction::I32GtS,
                BinOp::Le => Instruction::I32LeS,
                BinOp::Ge => Instruction::I32GeS,
                _ => unreachable!("non-relational ops handled elsewhere"),
            };
            function.instruction(&str_cmp);
            function.instruction(&Instruction::Else);
            // ELSE: numeric compare — ToNumber both operands, unbox to f64.
            emit_coerce_number(function, s0, helpers);
            emit_coerce_number(function, s1, helpers);
            function.instruction(&Instruction::LocalGet(s0));
            function.instruction(&Instruction::F64ReinterpretI64);
            function.instruction(&Instruction::LocalGet(s1));
            function.instruction(&Instruction::F64ReinterpretI64);
            let num_cmp = match op {
                BinOp::Lt => Instruction::F64Lt,
                BinOp::Gt => Instruction::F64Gt,
                BinOp::Le => Instruction::F64Le,
                BinOp::Ge => Instruction::F64Ge,
                _ => unreachable!("non-relational ops handled elsewhere"),
            };
            function.instruction(&num_cmp);
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
        // Abstract-Equality coercion; box its i32 result (negated for `!=`) as a
        // boolean jsval via `select` over TRUE/FALSE.
        BinOp::LooseEq | BinOp::LooseNe => {
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            emit_node(lhs, function, scratch, strings, helpers);
            emit_node(rhs, function, scratch, strings, helpers);
            function.instruction(&Instruction::Call(helpers.loose_eq));
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
