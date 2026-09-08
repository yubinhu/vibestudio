import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

// Vite config for the React SPA (client/web). `@` -> ./client/web.
// index.html + this config stay at the repo root (the Vite root); only the
// source tree moved to client/web/.
export default defineConfig(({ mode }) => ({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./client/web", import.meta.url)) },
  },
  // Tauri expects a fixed dev server.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // The browser dev UI talks to the backend (skill-server) purely over HTTP —
    // identical to the remote/separated deployment. Point VITE_API_TARGET at a
    // backend on another machine for the VS Code-remote-style split.
    proxy: {
      "/api": {
        // Native dev has a separate client switchboard: a durable host may
        // already own 8765. Plain browser/mobile-dev still reaches that host.
        target: process.env.VITE_API_TARGET ?? `http://127.0.0.1:${mode === "native" ? 8767 : 8765}`,
        changeOrigin: true,
      },
    },
    watch: {
      // Don't watch the Rust shell/server crates from Vite.
      ignored: ["**/client/desktop/**", "**/server/**"],
    },
  },
  // Build for the WebKit/WebView2 engines Tauri ships.
  build: {
    target: ["es2022", "chrome110", "safari15"],
    sourcemap: false,
  },
}));
