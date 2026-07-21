import { defineConfig } from 'tsdown'

// Layout contract between `dist/` and `wasm/`:
//
//   packages/jsc/
//     dist/index.js          built entry (this config's output)
//     wasm/jsc_wasm.js       wasm-bindgen glue, built by
//     wasm/jsc_wasm_bg.wasm  scripts/build-wasm.sh (gitignored)
//
// The glue and the `.wasm` binary are NOT bundled: the entry references them as
// `../wasm/jsc_wasm.js` and `new URL('../wasm/jsc_wasm_bg.wasm', import.meta.url)`,
// and because `dist/` sits at the same depth as `src/`, those specifiers stay
// valid verbatim in the output. Both directories ship in the npm package.
// Unlike @cdlab/image (one artifact per subpath), jsc ships a single wasm
// module, so there is no per-feature fan-out.
export default defineConfig({
  entry: {
    index: 'src/index.ts',
  },
  format: ['esm'],
  platform: 'neutral',
  target: 'es2022',
  dts: true,
  clean: true,
  deps: {
    neverBundle: [/^node:/, /\/wasm\/jsc_wasm(\.js|_bg\.wasm)$/],
  },
})
