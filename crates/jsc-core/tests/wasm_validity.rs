//! Module validity: small programs compile to modules `wasmparser` accepts,
//! both shaken and with every runtime body emitted (`full_runtime`), so a
//! stack-typing error in any helper fails here without running the module.

mod common;
use jsc_core::emit::CompileOptions;

/// Programs that keep different slices of the runtime: the trivial ones every
/// program pays for, the `BigInt` paths (literal, `typeof`, `===`, `==`, a
/// `Map` key, rendering, error messages), a dynamic key (which disables
/// shaking) and binary data (whose collector and renderer arms are gated).
const PROGRAMS: [&str; 14] = [
    "1",
    "console.log(1)",
    "1n",
    "console.log(1n, -12345678901234567890n, [0n], Object(2n))",
    "typeof 1n",
    "1n === 1n",
    "1n == 1",
    "new Map([[1n, 'a']]).get(1n)",
    "try { +1n } catch (e) { console.log(e.message) }",
    "const o = { a: 1 }; const k = 'a'; console.log(o[k])",
    "console.log(new ArrayBuffer(8).byteLength)",
    "const v = new DataView(new ArrayBuffer(8)); v.setFloat64(0, 1.5); console.log(v.getFloat64(0), v)",
    "console.log(BigInt('0x1F').toString(36), BigInt.asIntN(8, 255n), BigInt.asUintN(65, -1n), Number(2n))",
    "try { new BigInt(1) } catch (e) { console.log(e.message, BigInt(1.5)) }",
];

fn validate(src: &str, full_runtime: bool, gc_stress: u32) {
    // `compile_with` fails the test on an invalid module.
    let compiled = common::compile_with(
        src,
        CompileOptions {
            print_completion: true,
            gc_stress,
            no_stack_guard: false,
            full_runtime,
        },
    );
    assert!(
        compiled.is_ok(),
        "`{src}` failed to compile: {:?}",
        compiled.err()
    );
}

#[test]
fn shaken_modules_validate() {
    for src in PROGRAMS {
        validate(src, false, 0);
    }
}

#[test]
fn full_runtime_modules_validate() {
    for src in PROGRAMS {
        validate(src, true, 0);
        validate(src, true, 1);
    }
}
