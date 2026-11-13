import { defineConfig } from "vite";

// Tauri expects a fixed dev-server port.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "localhost" },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
