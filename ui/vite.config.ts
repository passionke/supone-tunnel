import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import legacy from "@vitejs/plugin-legacy";

// Author: kejiqing
export default defineConfig({
  plugins: [
    react(),
    // Android-x86 镜像 WebView≈Chrome 75：必须降级 ?. / ?? 等语法。
    legacy({
      targets: ["chrome >= 75"],
      modernPolyfills: true,
    }),
  ],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: ["es2019", "chrome75"],
    minify: "esbuild",
    outDir: "dist",
  },
});
