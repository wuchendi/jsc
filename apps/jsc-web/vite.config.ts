import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  // `@cdlab/jsc` loads its wasm-bindgen artifact via
  // `new URL('../wasm/jsc_wasm_bg.wasm', import.meta.url)` + `fetch`. Excluding it
  // from dep pre-bundling lets Vite process the package from source so the
  // `*_bg.wasm` file is emitted as a build asset and served in dev.
  optimizeDeps: {
    exclude: ['@cdlab/jsc'],
  },
  server: {
    host: '0.0.0.0',
    // Allow the nsl proxy's public dev domain through Vite's host check.
    allowedHosts: ['.a.wd.ds.cc'],
  },
})
