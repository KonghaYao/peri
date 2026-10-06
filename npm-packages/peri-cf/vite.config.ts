import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), cloudflare()],
  resolve: { dedupe: ["yjs", "lib0"] },
  server: { host: "127.0.0.1" },
});
