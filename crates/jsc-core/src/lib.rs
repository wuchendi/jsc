#![forbid(unsafe_code)]
//! Core library for the `jsc` JavaScript-to-WASM compiler.
//!
//! This crate holds the whole compilation pipeline that turns JavaScript source
//! text into an executed result. Source flows through four stages:
//!
//! 1. [`frontend`] — parse the source into an swc [`swc_ecma_ast::Program`].
//! 2. [`codegen`] — lower that AST into the crate's own [`ir::IrProgram`].
//! 3. [`emit`] — encode the IR into the bytes of a WebAssembly module.
//! 4. [`run`] — instantiate the module with `wasmtime` and decode its result.
//!
//! [`run::compile_and_run`] wires the stages together end to end. [`ir`] defines
//! the intermediate representation shared by codegen and emit, and [`error`]
//! carries source-spanned compile diagnostics across the stages.

mod abi;
pub mod codegen;
pub mod emit;
pub mod error;
pub mod frontend;
pub mod ir;
#[cfg(feature = "native-run")]
pub mod run;

#[cfg(test)]
mod tests;
