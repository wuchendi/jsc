//! Run: execute an emitted WebAssembly command module with `wasmtime`.
//!
//! [`harness`] is the WASI runner; [`host`] and [`promise`] back the `env`
//! imports not yet replaced by guest code, and [`value`] decodes jsvals for them.

mod harness;
mod host;
mod promise;
mod value;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use harness::run_inspect;
pub use harness::{Capture, RunOutcome, Sinks, run};
