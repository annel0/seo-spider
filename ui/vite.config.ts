import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the API is the running `seo-spider ui`.
export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": "http://127.0.0.1:7070" } },
  build: { outDir: "dist", chunkSizeWarningLimit: 2000 },
});
