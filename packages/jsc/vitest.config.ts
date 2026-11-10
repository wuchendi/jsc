import { defineConfig } from 'vitest/config'

// Node-environment tests exercising compile()/run() directly from src/. The
// emitted module is instantiated against the WASI polyfill (src/wasi.ts)
// alone, so these run without a browser.
export default defineConfig({
  test: {
    include: ['tests/**/*.test.ts'],
  },
})
