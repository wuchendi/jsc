//! Run: execute an emitted WebAssembly command module with `wasmtime`.
//!
//! [`harness`] is the WASI runner: the four hand-written
//! `wasi_snapshot_preview1` imports, `_start`, and the `proc_exit` code.

mod harness;

#[cfg(test)]
mod tests;

pub use harness::{Capture, RunOutcome, Sinks, run};
#[cfg(test)]
pub(crate) use harness::{THROWN_PROBE_EXPORT, run_inspect};
