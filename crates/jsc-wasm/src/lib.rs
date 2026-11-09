//! WebAssembly boundary crate for `@cdlab/jsc`.
//!
//! Thin `wasm-bindgen` glue over [`jsc_core`] that exposes a single export:
//! [`compile`], which runs a JavaScript source string through the
//! frontend -> codegen -> emit pipeline and hands back the emitted WebAssembly
//! module bytes. It deliberately does NOT link the `run` stage (jsc-core is
//! taken with `default-features = false`, dropping `native-run`/wasmtime): the
//! emitted module is instantiated on the JS side, in the browser. Compile
//! failures are thrown as JS `Error`s whose message is the CLI's caret
//! diagnostic when a source span is known, or the bare message otherwise — no
//! panics.

use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use jsc_core::emit::CompileOptions;
use jsc_core::error::CompileError;

#[wasm_bindgen]
extern "C" {
    /// The options object of [`compile`].
    #[wasm_bindgen(typescript_type = "{ printCompletion?: boolean }")]
    pub type JsCompileOptions;

    /// `options.printCompletion`.
    #[wasm_bindgen(method, getter, js_name = printCompletion)]
    fn print_completion(this: &JsCompileOptions) -> Option<bool>;
}

/// Compile JavaScript `src` to the bytes of a WebAssembly module.
///
/// Runs [`jsc_core::frontend::parse`] -> [`jsc_core::codegen::lower`] ->
/// [`jsc_core::emit::emit`] and returns the emitted module bytes. Any parse or
/// codegen failure is surfaced as a `JsError` (a JS `Error`); when the
/// underlying [`CompileError`] carries a source span, the message is the
/// rustc/swc-style caret block the CLI prints, so the browser can show exactly
/// where compilation failed. With `printCompletion`, the module renders its
/// completion value to fd 1 after the final drain.
#[wasm_bindgen]
pub fn compile(src: &str, options: Option<JsCompileOptions>) -> Result<Vec<u8>, JsError> {
    let options = CompileOptions {
        print_completion: options
            .and_then(|options| options.print_completion())
            .unwrap_or(false),
    };
    compile_inner(src, options).map_err(|err| JsError::new(&render_error(&err, src)))
}

/// The pipeline itself: parse, lower, emit. Kept separate so [`compile`] only
/// concerns itself with turning the error into a `JsError`.
fn compile_inner(src: &str, options: CompileOptions) -> anyhow::Result<Vec<u8>> {
    let program = jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js)?;
    let ir = jsc_core::codegen::lower(&program)?;
    Ok(jsc_core::emit::emit(&ir, options))
}

/// Render a pipeline error into a diagnostic string. A spanned [`CompileError`]
/// renders the caret block (the source has no file, so it is labelled
/// `<source>`); anything else falls back to its plain message.
fn render_error(err: &anyhow::Error, src: &str) -> String {
    err.downcast_ref::<CompileError>()
        .and_then(|compile_err| compile_err.render(src, "<source>"))
        .unwrap_or_else(|| err.to_string())
}
