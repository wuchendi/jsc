//! Function-valued properties of the global object.

#[allow(clippy::wildcard_imports)]
use super::*;

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
