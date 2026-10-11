import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

// SDK `/portable` 重导出的准入模块引用 `crypto`；浏览器侧不消费该面，用显式失败的替身替换，
// 避免 Node 内置模块阻断打包（见 web/shims/nodeCrypto.ts）。Worker 不引用 SDK，不受影响。
const nodeCryptoShim = fileURLToPath(new URL("./web/shims/nodeCrypto.ts", import.meta.url));

export default defineConfig({
  plugins: [react(), cloudflare()],
  resolve: {
    dedupe: ["yjs", "lib0"],
    alias: { crypto: nodeCryptoShim, "node:crypto": nodeCryptoShim },
  },
  server: { host: "127.0.0.1" },
});
