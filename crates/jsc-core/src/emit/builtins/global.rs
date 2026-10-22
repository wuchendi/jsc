//! Function-valued properties of the global object.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::emit::realm::NumberTest;

/// Store `ToNumber(argument 0)` into `n`, returning on a pending throw.
fn number_arg(function: &mut Function, frame: Frame, n: u32, helpers: Helpers<'_>) {
    frame.arg(function, 0);
    wasm!(function; Call(helpers.to_number), LocalSet(n));
    return_if_pending(function);
}

/// Turn the i32 condition on the stack into a boolean jsval.
fn push_bool(function: &mut Function) {
    wasm!(function;
        If(BlockType::Result(ValType::I64)),
        I64Const(TRUE_BITS), Else, I64Const(FALSE_BITS), End,
    );
}

/// `isNaN(number)`: whether `ToNumber(number)` is NaN.
pub(super) fn emit_is_nan(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let n = frame.i64(0);
    number_arg(function, frame, n, helpers);
    wasm!(function;
        LocalGet(n), F64ReinterpretI64, LocalGet(n), F64ReinterpretI64, F64Ne,
    );
    push_bool(function);
}

/// `isFinite(number)`: whether `ToNumber(number)` is neither NaN nor an
/// infinity (`n - n` is 0 exactly for finite `n`).
pub(super) fn emit_is_finite(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let n = frame.i64(0);
    number_arg(function, frame, n, helpers);
    wasm!(function;
        LocalGet(n), F64ReinterpretI64, LocalGet(n), F64ReinterpretI64, F64Sub,
        F64Const(0.0.into()), F64Eq,
    );
    push_bool(function);
}

/// `Number.isInteger` / `isSafeInteger` / `isFinite` / `isNaN`: the test on a
/// number argument, `false` for anything else (no coercion). A non-number
/// jsval reads as a NaN `f64`, which fails every test but `isNaN`'s.
pub(super) fn emit_number_is(test: NumberTest, function: &mut Function, frame: Frame) {
    let v = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    let finite = |function: &mut Function| {
        wasm!(function;
            LocalGet(v), F64ReinterpretI64, LocalGet(v), F64ReinterpretI64, F64Sub,
            F64Const(0.0.into()), F64Eq,
        );
    };
    let integral = |function: &mut Function| {
        finite(function);
        wasm!(function;
            LocalGet(v), F64ReinterpretI64, F64Trunc, LocalGet(v), F64ReinterpretI64, F64Eq,
            I32And,
        );
    };
    match test {
        NumberTest::NaN => {
            emit_is_number(function, v);
            wasm!(function;
                LocalGet(v), F64ReinterpretI64, LocalGet(v), F64ReinterpretI64, F64Ne, I32And,
            );
        }
        NumberTest::Finite => finite(function),
        NumberTest::Integer => integral(function),
        NumberTest::SafeInteger => {
            integral(function);
            wasm!(function;
                LocalGet(v), F64ReinterpretI64, F64Abs,
                F64Const(9_007_199_254_740_991.0.into()), F64Le, I32And,
            );
        }
    }
    push_bool(function);
}

/// `parseFloat(string)` / `parseInt(string, radix)`: the conversion
/// `parseFloat(s)` / `parseInt(s)` calls lower to, over the arguments.
pub(super) fn emit_parse(
    int: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, radix) = (frame.i64(0), frame.i64(1));
    frame.arg(function, 0);
    wasm!(function; LocalSet(s));
    if int {
        frame.arg(function, 1);
        wasm!(function; LocalSet(radix));
    }
    let node = IrNode::ConvertCall {
        op: if int {
            ConvertOp::ParseInt
        } else {
            ConvertOp::ParseFloat
        },
        arg: Box::new(IrNode::Local(s)),
        radix: int.then(|| Box::new(IrNode::Local(radix))),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}
