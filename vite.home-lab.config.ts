import { mergeConfig } from "vite";
import config from "./vite.config";

export default mergeConfig(config, {
  server: { port: 1421, strictPort: true },
  build: {
    outDir: "build/home-lab",
    rollupOptions: { input: "home-lab.html" },
  },
});
