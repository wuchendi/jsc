//! Module validity: small programs compile to modules `wasmparser` accepts,
//! both shaken and with every runtime body emitted (`full_runtime`), so a
//! stack-typing error in any helper fails here without running the module.

mod common;
use jsc_core::emit::CompileOptions;

/// Programs that keep different slices of the runtime: the trivial ones every
/// program pays for, the `BigInt` paths (literal, `typeof`, `===`, `==`, a
/// `Map` key, rendering, error messages), a dynamic key (which disables
/// shaking), binary data (whose collector and renderer arms are gated),
/// the numeric operators on `BigInts` and numbers, the `BigInt` built-ins,
/// and the `BigInt` gate (DESIGN.md §6.10.9): every gated operator site with
/// the gate closed, and with it opened by the global object or a name only.
const PROGRAMS: [&str; 21] = [
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
    "let x = 2n ** 64n; x++; console.log(x / 3n, x % 3n, -x, ~x, x << 3n, x >> 3n, x & -5n, x < 2 ** 64, x > '1')",
    "let a = 1; a = -a; a++; a--; console.log(a ** 2, a >>> 1, ~a, a % 3, a - 1, a * 2, a / 2)",
    "const o = { v: 1n }; o.v **= 3n; o.v--; globalThis.g = 1n; g++; let c = 1n; (() => c++)(); console.log(o.v, g, c)",
    "console.log(BigInt('0x1F').toString(36), BigInt.asIntN(8, 255n), BigInt.asUintN(65, -1n), Number(2n))",
    "try { new BigInt(1) } catch (e) { console.log(e.message, BigInt(1.5)) }",
    "const o = { n: 1 }; let c = 3; (() => c++)(); o.n++; --o['n']; o.n **= 2; let a = -o.n; a = ~a; \
     a = a ** 2 - '2' * 3 / 4 % 5; a = (a & 1) | (a ^ 2) << 1 >> 1 >>> 0; \
     class K { #p = 1; inc() { return this.#p++; } } function* g() { let i = 0; i++; yield i; } \
     console.log(a < '3', a >= 1, a + 'x', typeof a, !!a, a === 1, Number('4'), c, new K().inc(), [...g()])",
    "class A { get x() { return 1; } set x(v) {} } \
     class B extends A { m() { super.x++; return --super.x; } } \
     let x = 1; x--; console.log(new B().m(), x > 0, 1 / 0, -0 === 0, typeof x)",
    "const k = 'Big' + 'Int'; const B = globalThis[k]; let x = B(5); x++; \
     console.log(-x, ~x, x ** 2n, x < 7, typeof x, !!x, x === 6n, Number(x))",
    "const o = { BigInt: 1 }; let a = 1; a++; console.log(-a, ~a, a ** 2, a < 3, o.BigInt)",
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
