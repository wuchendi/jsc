//! Integration tests: string milestones and truthiness via the CLI.

use std::process::Command;
mod common;
use common::jsc;

/// The S10 string milestones run inline with `-e` through the built binary.
///
/// Covers every string wave end to end: `typeof`, `.length`, `+` concat,
/// strict `===`/`!==`, single-char indexing, and a length-of-concat chain.
/// Concatenated output prints raw (no surrounding quotes).
#[test]
fn string_milestones_via_eval() {
    assert_eq!(jsc(&["-e", r#"typeof "hi""#]), "string");
    assert_eq!(jsc(&["-e", r#""foo".length"#]), "3");
    assert_eq!(jsc(&["-e", r#""foo" + "bar""#]), "foobar");
    assert_eq!(jsc(&["-e", r#""a" === "a""#]), "true");
    assert_eq!(jsc(&["-e", r#""a" === "b""#]), "false");
    assert_eq!(jsc(&["-e", r#""a" !== "b""#]), "true");
    assert_eq!(jsc(&["-e", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-e", r#"("ab" + "cd").length"#]), "4");
}

/// String truthiness drives `if`: `""` is falsy, any non-empty string is truthy.
///
/// The result is captured through a numeric var so the branch taken is visible
/// in the printed line.
#[test]
fn string_truthiness_selects_branch() {
    assert_eq!(
        jsc(&["-e", r#"let s=""; let r=0; if(s){r=1}else{r=2} r"#]),
        "2",
    );
    assert_eq!(
        jsc(&["-e", r#"let s="x"; let r=0; if(s){r=1}else{r=2} r"#]),
        "1",
    );
}

/// P0.1 — template literals lower to `+` concatenation, reusing the string
/// coercion path so interpolated numbers render like `"n="+42`.
///
/// Covers all five milestones: interpolated arithmetic, a variable read, the
/// no-interpolation and empty forms, adjacent exprs with empty middle quasis,
/// and a nested template inside an interpolation.
#[test]
fn template_literals_lower_to_concat() {
    // 1. interpolated arithmetic coerces the number result to a string
    assert_eq!(jsc(&["-e", r"`hi ${40+2}`"]), "hi 42");
    // 2. a variable read inside the template
    assert_eq!(jsc(&["-e", r"let x=5; `x=${x}`"]), "x=5");
    // 3. no-interpolation and empty templates stay strings
    assert_eq!(jsc(&["-e", r"`abc`"]), "abc");
    assert_eq!(jsc(&["-e", r"`` + ``"]), "");
    // 4. adjacent exprs with empty middle quasis
    assert_eq!(jsc(&["-e", r"`${1}${2}${3}`"]), "123");
    // 5. nested template inside an interpolation
    assert_eq!(jsc(&["-e", r"`a${`b${1}`}c`"]), "ab1c");
    // a lone interpolation still coerces to a string
    assert_eq!(jsc(&["-e", r"`${5}`"]), "5");
}

/// M2 — the added string methods run inline with `-e` end to end: trimming,
/// repeat, prefix/suffix and substring tests, padding, substring, and split.
#[test]
fn m2_string_methods_via_eval() {
    assert_eq!(jsc(&["-e", r#""  hi  ".trim()"#]), "hi");
    assert_eq!(jsc(&["-e", r#""ab".repeat(3)"#]), "ababab");
    assert_eq!(jsc(&["-e", r#""hello".startsWith("he")"#]), "true");
    assert_eq!(jsc(&["-e", r#""hello".endsWith("lo")"#]), "true");
    assert_eq!(jsc(&["-e", r#""hello".includes("ell")"#]), "true");
    assert_eq!(jsc(&["-e", r#""5".padStart(3,"0")"#]), "005");
    assert_eq!(jsc(&["-e", r#""hello".substring(3,1)"#]), "el");
    assert_eq!(jsc(&["-e", r#""a,b,c".split(",").length"#]), "3");
    // methods chain with the pre-existing ones
    assert_eq!(jsc(&["-e", r#""  hi  ".trim().toUpperCase()"#]), "HI");
}

/// P0.1 — tagged templates remain unsupported: the CLI exits non-zero and the
/// diagnostic names them.
#[test]
fn tagged_template_is_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", r"tag`hi ${1}`"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        !output.status.success(),
        "a tagged template must make jsc exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tagged template"),
        "stderr should explain the tagged-template rejection, got: {stderr}"
    );
}
