import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // web/dev holds demo catalogue output that the demo script rewrites; never watch it.
  server: { port: 3230, strictPort: true, watch: { ignored: ["**/dev/**"] } },
  preview: { port: 3230, strictPort: true },
});
