import { defineConfig, type Connect } from "vite";
import react from "@vitejs/plugin-react";

// Like the deployed store (scripts/build-sites.mjs), only the play page is
// cross-origin isolated; the other pages can embed YouTube trailers.
const isolatePlay: Connect.NextHandleFunction = (req, res, next) => {
  if (req.url?.startsWith("/play/")) {
    res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    res.setHeader("Cross-Origin-Embedder-Policy", "credentialless");
  }
  next();
};

export default defineConfig({
  plugins: [
    react(),
    {
      name: "isolate-play",
      configureServer: (server) => void server.middlewares.use(isolatePlay),
      configurePreviewServer: (server) => void server.middlewares.use(isolatePlay),
    },
  ],
  clearScreen: false,
  // web/dev holds demo catalogue output that the demo script rewrites; never watch it.
  server: { port: 3230, strictPort: true, watch: { ignored: ["**/dev/**"] } },
  preview: { port: 3230, strictPort: true },
});
