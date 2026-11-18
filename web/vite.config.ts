import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The same cross-origin isolation headers as the deployed store (scripts/build-sites.mjs).
const headers = { "Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "credentialless" };

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // web/dev holds demo catalogue output that the demo script rewrites; never watch it.
  server: { port: 3230, strictPort: true, headers, watch: { ignored: ["**/dev/**"] } },
  preview: { port: 3230, strictPort: true, headers },
});
