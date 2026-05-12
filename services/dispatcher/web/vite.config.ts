import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // Proxy /api calls to the FastAPI dev server. The dispatcher serves
    // both API + built SPA from port 8080 in production; in dev we run
    // vite separately so HMR works.
    proxy: {
      "/api": "http://localhost:8080",
      "/healthz": "http://localhost:8080",
    },
  },
  build: {
    // FastAPI mounts `web/dist` as a StaticFiles route. Keep the path
    // stable so prod images don't need to be reconfigured.
    outDir: "dist",
    emptyOutDir: true,
  },
});
