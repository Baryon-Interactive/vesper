import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  define: { "import.meta.env.VITE_VESPER_DEMO_TOUR": JSON.stringify(process.env.VESPER_DEMO_TOUR === "1") },
  server: { port: 1420, strictPort: true },
});
