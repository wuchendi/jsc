//! Run: execute an emitted WebAssembly command module with `wasmtime`.
//!
//! [`harness`] is the WASI runner; [`promise`] backs the `env` imports not yet
//! replaced by guest code.

mod harness;
mod promise;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use harness::run_inspect;
pub use harness::{Capture, RunOutcome, Sinks, run};
