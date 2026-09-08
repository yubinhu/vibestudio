import { defineConfig, mergeConfig } from "vite";
import config from "./vite.config";

export default defineConfig((env) => mergeConfig(config(env), {
  server: { port: 1421, strictPort: true },
  build: {
    outDir: "build/home-lab",
    rollupOptions: { input: "home-lab.html" },
  },
}));
