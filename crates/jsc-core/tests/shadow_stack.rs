//! Integration tests: the shadow-stack frames placed by the function builder
//! (no collector yet, so a frame must be inert and balanced).
#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{assert_number, assert_output, assert_uncaught, compile, fails};
use wasmparser::{Operator, Parser, Payload};
use wasmtime::{Config, Engine, Linker, Module, Store, Trap};

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
        frame_slots(&bodies(&program)[2]).unwrap_or(0)
    };
    assert_eq!(framed("g()"), framed("0") + 1);
}

/// A numeric operator's operands are stashed in raw locals: even in a loop
/// holding a safepoint, the two stashes follow each other with no slot write.
#[test]
fn numeric_scratch_not_written_through() {
    let bodies = bodies(
        "function g() { return 0; }
         function f(a, b) {
           var s = 0;
           for (var i = 0; i < 2; i = i + 1) { g(); s = a * b; }
           return s;
         }
         f(2, 3)",
    );
    assert!(bodies[2].windows(3).any(|w| matches!(
        w,
        [
            Operator::LocalSet { local_index: s1 },
            Operator::LocalSet { local_index: s0 },
            Operator::LocalGet { local_index: read },
        ] if *s0 + 1 == *s1 && read == s0
    )));
}

/// `b` can still be a heap value while `ToNumber(a)` runs, so it takes a temp
/// slot in the cold arm; a numeric right operand needs none.
#[test]
fn numeric_coerce_roots_pending_operand() {
    let framed = |rhs: &str| {
        let program = format!("function f(a, b) {{ return a * {rhs}; }} f(2, 3)");
        frame_slots(&bodies(&program)[1])
    };
    assert_eq!(framed("b"), Some(1));
    assert_eq!(framed("2"), None);
}

/// Every `Math.min` argument stays rooted until folded: the later ones are
/// heap values while the first one's `ToNumber` runs.
#[test]
fn min_max_roots_stashed_arguments() {
    let framed = |args: &str| {
        let program = format!(
            "function g(x) {{ return x; }} function f() {{ return Math.min({args}); }} f()"
        );
        frame_slots(&bodies(&program)[2]).unwrap_or(0)
    };
    assert_eq!(framed("g(1), g(2)"), framed("g(1) * 1, g(2) * 1") + 2);
}

/// A kind test's base is never rooted, even in a loop holding a safepoint: the
/// tee is followed directly by the header load, with no write-through.
#[test]
fn proto_kind_test_tmp_is_raw() {
    let bodies = bodies(
        "function g() { return 0; }
         function f(a) { for (var i = 0; i < 2; i = i + 1) { a.push(i); g(); } return a; }
         f([])",
    );
    assert!(bodies[2].windows(3).any(|w| matches!(
        w,
        [
            Operator::I32WrapI64,
            Operator::LocalTee { .. },
            Operator::I64Load { memarg },
        ] if memarg.offset == 0
    )));
}

#[test]
fn frame_slots_cleared_on_entry() {
    // Few slots: one `i64.store` of zero per slot right after the prologue.
    let small = bodies(
        "function g(x) { return x; }
         function f(a) { var y = g(a); g(y); return a + y; }
         f(1)",
    );
    let n = frame_slots(&small[2]).expect("framed") as usize;
    assert!(n <= 4, "expected a small frame, got {n}");
    // Every slot is written once: cleared, or filled by its parameter's store.
    let (mut offsets, mut cleared, mut filled) = (Vec::new(), 0, 0);
    for k in 0..n {
        let at = 6 + 3 * k;
        assert!(matches!(small[2][at], Operator::LocalGet { .. }));
        match small[2][at + 1] {
            Operator::I64Const { value: 0 } => cleared += 1,
            Operator::LocalGet { .. } => filled += 1,
            ref other => panic!("not a slot write: {other:?}"),
        }
        let Operator::I64Store { memarg } = small[2][at + 2] else {
            panic!("not a slot store");
        };
        offsets.push(memarg.offset);
    }
    offsets.sort_unstable();
    assert_eq!(
        offsets,
        (0..n as u64).map(|k| 8 + 8 * k).collect::<Vec<_>>()
    );
    assert!(
        cleared > 0 && filled > 0,
        "{cleared} cleared, {filled} filled"
    );
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
    assert!(matches!(big[2][7], Operator::I32Const { value: 0 }));
    assert!(
        matches!(big[2][8], Operator::I32Const { value } if value.cast_unsigned() == 8 * n + 8)
    );
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

/// The three render reserves (output, recursion path, ref table) each build a
/// `TYPE_RAW` block, and rendering is unchanged once all three have grown.
#[test]
fn render_buffers_are_type_raw() {
    let reserves = bodies("1")
        .iter()
        .filter(|ops| {
            let typed = ops.windows(2).any(|w| {
                matches!(w[0], Operator::I32Const { value } if value == TYPE_RAW)
                    && matches!(w[1], Operator::I32Store8 { memarg } if memarg.offset == 0)
            });
            let copies = ops
                .iter()
                .any(|op| matches!(op, Operator::MemoryCopy { .. }));
            let calls = ops
                .iter()
                .filter(|op| matches!(op, Operator::Call { .. }))
                .count();
            typed && copies && calls == 1
        })
        .count();
    assert_eq!(reserves, 3, "out, path and refs reserves");

    // Output past the first 64-byte block.
    let long = "x".repeat(200);
    assert_output(&format!("console.log('{long}')"), &format!("{long}\n"));
    // Recursion path past 16 entries.
    let depth = 20;
    assert_output(
        &format!(
            "var a = []; for (var i = 0; i < {depth}; i = i + 1) {{ a = [a]; }} console.log(a)"
        ),
        &format!("{}[]{}\n", "[ ".repeat(depth), " ]".repeat(depth)),
    );
    // Ref table past 16 entries, with each `<ref *N>` rotated into place.
    let refs = 20;
    let items = (1..=refs)
        .map(|n| format!("<ref *{n}> {{ s: [Circular *{n}] }}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_output(
        &format!(
            "var a = [];
             for (var i = 0; i < {refs}; i = i + 1) {{ var o = {{}}; o.s = o; a.push(o); }}
             console.log(a)"
        ),
        &format!("[ {items} ]\n"),
    );
}

/// `Math.sumPrecise` still sums through its `TYPE_RAW` accumulator.
#[test]
fn sum_precise_uses_raw_accumulator() {
    assert_number("Math.sumPrecise([1, 2, 3])", 6.0);
}

/// `TYPE_ENV`, the environment-block type byte (`abi.rs`).
const TYPE_ENV: i32 = 4;

/// The `Array.fromAsync` state block: 10 slots behind the 8-byte header.
const FA_SIZE: i32 = 88;
const FA_SLOTS: i32 = 10;

/// `Array.fromAsync` keeps its state in a `TYPE_ENV` block and passes the
/// block as a heap jsval, so env tracing sees both.
#[test]
fn from_async_state_block_is_headered_env() {
    let heap = 0x7FFD_0000_0000_0000_u64.cast_signed();
    let bodies = bodies("Array.fromAsync([1])");
    let entry = bodies
        .iter()
        .find_map(|ops| {
            let at = ops.windows(2).position(|w| {
                matches!(w[0], Operator::I32Const { value } if value == FA_SIZE)
                    && matches!(w[1], Operator::Call { .. })
            })?;
            let header = &ops[at..(at + 24).min(ops.len())];
            let typed = header.windows(2).any(|w| {
                matches!(w[0], Operator::I32Const { value } if value == TYPE_ENV)
                    && matches!(w[1], Operator::I32Store8 { memarg } if memarg.offset == 0)
            });
            let counted = header.windows(2).any(|w| {
                matches!(w[0], Operator::I32Const { value } if value == FA_SLOTS)
                    && matches!(w[1], Operator::I32Store { memarg } if memarg.offset == 4)
            });
            (typed && counted).then_some(ops)
        })
        .expect("no TYPE_ENV fromAsync state block is written");
    // Into a local: a base local's own write-through ends in an `i64.store`.
    let tagged = entry.windows(4).any(|w| {
        matches!(w[0], Operator::I64ExtendI32U)
            && matches!(w[1], Operator::I64Const { value } if value == heap)
            && matches!(w[2], Operator::I64Or)
            && matches!(w[3], Operator::LocalSet { .. } | Operator::LocalTee { .. })
    });
    assert!(tagged, "the state block must be passed as HEAP | base");
}

#[test]
fn deep_recursion_traps() {
    assert!(fails("function r(n) { return r(n + 1) + 1; } r(0)"));
}

/// A recursion whose frame is one slot (`a`, read after the call; the argument
/// temp is released before the call), so `$fp` steps by 8 through `0` and `-8`.
const ONE_SLOT_RECURSION: &str = "function r(a) { r(a); return a; } r(1)";

/// Native stack for [`exhaust`]: room for every one-slot frame of the 1 MiB
/// shadow stack. Wasmtime's default 512 KiB would overflow long before it.
const NATIVE_STACK: usize = 240 << 20;

/// Run `src` until it traps; the trap and the first 16 bytes of its memory.
/// Imports are stubbed: no program here reaches one before trapping.
fn exhaust(src: &str) -> (Trap, Vec<u8>) {
    let wasm = compile(src, false).expect("compiles");
    std::thread::Builder::new()
        .stack_size(NATIVE_STACK + (16 << 20))
        .spawn(move || {
            // `Engine::new` rejects a `max_wasm_stack` above `async_stack_size`,
            // even when no async entry point is used.
            let engine = Engine::new(
                Config::new()
                    .max_wasm_stack(NATIVE_STACK)
                    .async_stack_size(NATIVE_STACK),
            )
            .expect("engine");
            let module = Module::new(&engine, &wasm).expect("module");
            let mut store = Store::new(&engine, ());
            let mut linker = Linker::new(&engine);
            linker
                .define_unknown_imports_as_traps(&module)
                .expect("stub imports");
            let instance = linker.instantiate(&mut store, &module).expect("instance");
            let start = instance
                .get_typed_func::<(), ()>(&mut store, "_start")
                .expect("_start");
            let err = start.call(&mut store, ()).expect_err("should trap");
            let trap = *err.downcast_ref::<Trap>().expect("a wasm trap");
            let memory = instance.get_memory(&mut store, "memory").expect("memory");
            (trap, memory.data(&store)[..16].to_vec())
        })
        .expect("spawn")
        .join()
        .expect("runner thread")
}

#[test]
fn raw_ref_zero_reads_zero_when_stack_nearly_full() {
    let (_, memory) = exhaust(ONE_SLOT_RECURSION);
    assert_eq!(memory[..8], [0; 8], "stack data below the floor");
    assert_ne!(memory[8..16], [0; 8], "the stack never reached its floor");
}

#[test]
fn first_push_past_floor_traps() {
    assert_eq!(frame_slots(&bodies(ONE_SLOT_RECURSION)[1]), Some(1));
    let (trap, memory) = exhaust(ONE_SLOT_RECURSION);
    assert_eq!(trap, Trap::MemoryOutOfBounds);
    // `$fp = 0` wrote slot 0 at address 8, so the trap was the very next push.
    assert_eq!(memory[8..16], 1.0_f64.to_bits().to_le_bytes());
}
