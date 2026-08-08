import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Vite 8 + Vue 3 配置（供 Tauri 前端使用）
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: "0.0.0.0",
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: ["chrome105", "safari13"],
    sourcemap: false,
    outDir: "dist",
    emptyOutDir: true,
  },
});