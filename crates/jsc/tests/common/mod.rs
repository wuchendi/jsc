//! Shared harness helpers for the `jsc` CLI integration suites.
//!
//! Brought into each theme file via `mod common;`. Helpers are plain fns (not
//! `#[test]`), so `allow-expect-in-tests` does not reach them — allow `expect`
//! explicitly. Not every helper is used by every theme binary, so allow
//! `dead_code` too.
#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::process::Command;

/// Run the built `jsc` binary with `args` and return its trimmed stdout.
pub(crate) fn jsc(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(args)
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.success(),
        "jsc {args:?} exited with {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("stdout should be UTF-8")
        .trim()
        .to_owned()
}

/// Run the built `jsc` binary with `args` and return its RAW stdout (untrimmed),
/// so `console.log` newlines and separators can be asserted byte-for-byte.
pub(crate) fn jsc_raw_stdout(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(args)
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.success(),
        "jsc {args:?} exited with {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout should be UTF-8")
}
