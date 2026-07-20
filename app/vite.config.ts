import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { nodePolyfills } from 'vite-plugin-node-polyfills'

// nodePolyfills is REQUIRED: the Arch SDK pulls in Buffer/process via its crypto deps.
// Arch's own docs warn that hand-rolled shims fail because injection order matters.
export default defineConfig({
  plugins: [react(), nodePolyfills({ globals: { Buffer: true, process: true } })],
  server: { port: 5173 },
})
