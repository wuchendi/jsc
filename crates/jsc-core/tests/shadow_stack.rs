//! Integration tests: the shadow-stack frames placed by the function builder
//! (no collector yet, so a frame must be inert and balanced).
#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{assert_number, assert_uncaught, compile, fails};
use wasmparser::{Operator, Parser, Payload};

/// Every function body's operators, indexed by defined-function order (`main`
/// is 0, user function `i` is `i + 1`).
fn bodies(src: &str) -> Vec<Vec<Operator<'static>>> {
    let wasm: &'static [u8] = Box::leak(compile(src, false).expect("compiles").into_boxed_slice());
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("valid module") {
            let reader = body.get_operators_reader().expect("operators");
            out.push(
                reader
                    .into_iter()
                    .map(|op| op.expect("operator"))
                    .collect::<Vec<_>>(),
            );
        }
    }
    out
}

/// The slot count of a body's prologue, or `None` for a frame-free body.
fn frame_slots(ops: &[Operator<'_>]) -> Option<u32> {
    match ops {
        [
            Operator::GlobalGet { .. },
            Operator::LocalTee { .. },
            Operator::I32Const { value },
            Operator::I32Sub,
            Operator::LocalTee { .. },
            Operator::GlobalSet { .. },
            ..,
        ] => Some(value.cast_unsigned() / 8),
        _ => None,
    }
}

/// More frames than the 1 MiB shadow stack holds at 8 bytes a slot: a frame
/// left behind traps the loop.
const LOOPS: u32 = 200_000;

#[test]
fn shadow_sp_restored_after_return() {
    assert_number(
        &format!(
            "function h(x) {{ return x; }}
             function g(x) {{ var y = h(x); return x + y; }}
             var s = 0;
             for (var i = 0; i < {LOOPS}; i = i + 1) {{ s = g(1); }}
             s"
        ),
        2.0,
    );
}

#[test]
fn shadow_sp_restored_after_uncaught_throw() {
    // The throw leaves `t` with no handler in it; the caller catches.
    assert_number(
        &format!(
            "function h(x) {{ return x; }}
             function t(x) {{ var y = h(x); if (x >= 0) throw y; return x + y; }}
             var s = 0;
             for (var i = 0; i < {LOOPS}; i = i + 1) {{
               try {{ t(1); }} catch (e) {{ s = e; }}
             }}
             s"
        ),
        1.0,
    );
    assert_uncaught(
        "function h(x) { return x; } function t(x) { var y = h(x); throw y + x; } t(1)",
        "2",
    );
}

#[test]
fn shadow_sp_restored_after_caught_throw() {
    assert_number(
        &format!(
            "function h(x) {{ return x; }}
             function c(x) {{
               var y = 0;
               try {{ y = h(x); throw y; }} catch (e) {{ y = e; }}
               return x + y;
             }}
             var s = 0;
             for (var i = 0; i < {LOOPS}; i = i + 1) {{ s = c(1); }}
             s"
        ),
        2.0,
    );
}

#[test]
fn leaf_function_has_no_frame() {
    let bodies = bodies("function leaf(a) { return a; } leaf(1)");
    assert_eq!(frame_slots(&bodies[1]), None);
}

/// The same loop with a counter that can hold a heap value needs one more
/// slot: the numeric counter is `NoRoot`.
#[test]
fn noroot_loop_counter_not_spilled() {
    let framed = |init: &str| {
        let program = format!(
            "function g() {{ return 0; }}
             function f() {{
               var i = {init};
               while (i < 3) {{ g(); i = i + 1; }}
               return i;
             }}
             f()"
        );
        frame_slots(&bodies(&program)[2]).expect("the call is a safepoint")
    };
    assert_eq!(framed("g()"), framed("0") + 1);
}

#[test]
fn frame_slots_cleared_on_entry() {
    // Few slots: one `i64.store` of zero per slot right after the prologue.
    let small = bodies(
        "function g(x) { return x; }
         function f(a) { var y = g(a); g(y); return a; }
         f(1)",
    );
    let n = frame_slots(&small[2]).expect("framed") as usize;
    assert!(n <= 4, "expected a small frame, got {n}");
    for k in 0..n {
        let at = 6 + 3 * k;
        assert!(matches!(small[2][at], Operator::LocalGet { .. }));
        assert!(matches!(small[2][at + 1], Operator::I64Const { value: 0 }));
        assert!(matches!(small[2][at + 2], Operator::I64Store { .. }));
    }
    // Many slots: a `memory.fill` instead.
    let big = bodies(
        "function g(x) { return x; }
         function f(a, b, c, d, e) {
           var y = g(a);
           return [a, b, c, d, e, y, g(b), g(c), g(d), g(e)];
         }
         f(1, 2, 3, 4, 5)",
    );
    let n = frame_slots(&big[2]).expect("framed");
    assert!(n > 4, "expected a large frame, got {n}");
    assert!(matches!(big[2][9], Operator::MemoryFill { .. }));
}

/// The async-generator queue link holds a heap jsval, so env tracing sees it.
#[test]
fn async_gen_request_next_is_heap_jsval() {
    let heap = 0x7FFD_0000_0000_0000_u64.cast_signed();
    let tagged = bodies("1").iter().any(|ops| {
        ops.windows(4).any(|w| {
            matches!(w[0], Operator::I64ExtendI32U)
                && matches!(w[1], Operator::I64Const { value } if value == heap)
                && matches!(w[2], Operator::I64Or)
                && matches!(
                    w[3],
                    Operator::I64Store { memarg } if memarg.offset == REQ_NEXT_OFFSET
                )
        })
    });
    assert!(tagged, "`next` must be stored as HEAP | record");
}

/// `TYPE_RAW`, the internal raw-buffer type byte (`abi.rs`).
const TYPE_RAW: i32 = 19;

/// The byte offset of a request record's `next` slot (`REQ_VALUE + 24`, with `REQ_VALUE = 8`).
const REQ_NEXT_OFFSET: u64 = 32;

/// Growing the intern table builds a `TYPE_RAW` block, and interning keeps
/// working across the growth.
#[test]
fn intern_growth_block_is_type_raw() {
    let has_raw_store = bodies("1").iter().any(|ops| {
        ops.windows(2).any(|w| {
            matches!(
                w[0],
                Operator::I32Const { value } if value == TYPE_RAW
            ) && matches!(w[1], Operator::I32Store8 { .. })
        })
    });
    assert!(has_raw_store, "no TYPE_RAW block is written");
    assert_number(
        "var o = {};
         for (var i = 0; i < 5000; i = i + 1) { o['k' + i] = i; }
         o['k4999'] + o['k0']",
        4999.0,
    );
}

/// `Math.sumPrecise` still sums through its `TYPE_RAW` accumulator.
#[test]
fn sum_precise_uses_raw_accumulator() {
    assert_number("Math.sumPrecise([1, 2, 3])", 6.0);
}

#[test]
fn deep_recursion_traps() {
    assert!(fails("function r(n) { return r(n + 1) + 1; } r(0)"));
}
