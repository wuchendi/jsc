//! Integration tests: the TypeScript dialect, selected by file extension and
//! compiled by stripping types before codegen.
#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};
mod common;
use common::jsc;

/// Write `src` to `name` under a per-test directory in the cargo target tmp
/// dir and return its path.
fn write_source(dir: &str, name: &str, src: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("typescript")
        .join(dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    let path = dir.join(name);
    std::fs::write(&path, src).expect("write source file");
    path
}

fn run_jsc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(args)
        .output()
        .expect("failed to spawn jsc binary")
}

const TYPED: &str = r#"
interface Point { x: number; y: number }
type Name = string;
function add(a: number, b: number): number { return a + b; }
function first<T>(xs: T[]): T { return xs[0]; }
const p: Point = { x: 1, y: 2 };
let label: Name = "sum";
const maybe: number | undefined = add(p.x, p.y);
const n = maybe!;
const s = first<string>(["a", "b"]) as string;
console.log(label, n, s);
"#;

#[test]
fn ts_file_runs_with_types_stripped() {
    let path = write_source("run", "typed.ts", TYPED);
    assert_eq!(jsc(&[path.to_str().expect("utf-8 path")]), "sum 3 a");
}

#[test]
fn mts_and_cts_files_use_ts_dialect() {
    for name in ["typed.mts", "typed.cts"] {
        let path = write_source("ext", name, "let x: number = 41;\nconsole.log(x + 1);\n");
        assert_eq!(jsc(&[path.to_str().expect("utf-8 path")]), "42", "{name}");
    }
}

#[test]
fn build_ts_file_emits_runnable_module() {
    let src = write_source("build", "typed.ts", TYPED);
    let out = src.with_extension("wasm");
    let output = run_jsc(&[
        "build",
        src.to_str().expect("utf-8 path"),
        "-o",
        out.to_str().expect("utf-8 path"),
    ]);
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&out).expect("read module");
    assert!(bytes.starts_with(b"\0asm"), "output must be a wasm module");
    jsc_core::run::run(&bytes).expect("built module must instantiate and run");
}

#[test]
fn tsx_file_is_a_compile_error() {
    let path = write_source("tsx", "view.tsx", "const x: number = 1;\n");
    let output = run_jsc(&[path.to_str().expect("utf-8 path")]);
    assert_eq!(output.status.code(), Some(2), "a .tsx file must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TSX") && stderr.contains("not supported"),
        "stderr should explain TSX is unsupported, got: {stderr}"
    );
}

#[test]
fn print_mode_stays_javascript() {
    let output = run_jsc(&["-p", "let x: number = 1; x"]);
    assert_eq!(output.status.code(), Some(2), "-p must parse as JavaScript");
}

#[test]
fn js_file_rejects_type_annotations() {
    let path = write_source("js", "typed.js", "let x: number = 1;\n");
    let output = run_jsc(&[path.to_str().expect("utf-8 path")]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a .js file must parse as JavaScript"
    );
}

/// Assert that running `src` as a `.ts` file exits 0 printing `expected`.
fn assert_ts_output(dir: &str, src: &str, expected: &str) {
    let path = write_source(dir, "input.ts", src);
    let output = run_jsc(&[path.to_str().expect("utf-8 path")]);
    assert_eq!(output.status.code(), Some(0), "must run");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// `strip` lowers an `enum` to `var E = function (E) { .. }(E || {})`: the
/// hoisted `var` reads `undefined` inside its own initializer.
#[test]
fn ts_enum_runs() {
    assert_ts_output(
        "enum",
        "enum Color { Red, Green = 5, Blue }\nconsole.log(Color.Red, Color.Blue, Color[5]);\n",
        "0 6 Green\n",
    );
}

/// `strip` lowers a `namespace` to an IIFE followed by a trailing `var NS;`,
/// which is hoisted above the IIFE.
#[test]
fn ts_namespace_runs() {
    assert_ts_output(
        "namespace",
        "namespace NS { export const v = 7; }\nconsole.log(NS.v);\n",
        "7\n",
    );
}
