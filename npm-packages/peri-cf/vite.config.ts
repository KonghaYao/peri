import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

// 应用源码按 `@/...` 约定导入前端根。
const frontendRoot = fileURLToPath(new URL("./frontend", import.meta.url));

// SDK `/portable` 的准入模块引用 `crypto`；浏览器侧不消费该面，用显式失败的替身替换。
const nodeCryptoShim = fileURLToPath(new URL("./frontend/shims/nodeCrypto.ts", import.meta.url));

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    // `@peri-code/sdk/openui` 以源文件引入；与主应用共用同一份 React。
    dedupe: ["react", "react-dom"],
    alias: {
      "@": frontendRoot,
      crypto: nodeCryptoShim, "node:crypto": nodeCryptoShim,
    },
  },
  server: {
    host: "127.0.0.1",
    proxy: { "/api": { target: "http://127.0.0.1:8787", ws: true } },
  },
  build: { outDir: "dist/client", emptyOutDir: true },
});
