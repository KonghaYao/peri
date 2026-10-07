import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import { unstable_readConfig } from "wrangler";
import { localDnsOverrides } from "./scripts/local-dns";

export default defineConfig(async ({ command, isPreview }) => {
  let dnsOverrides: string | undefined;
  if (command === "serve" && !isPreview) {
    const configPath = fileURLToPath(new URL("./wrangler.jsonc", import.meta.url));
    const { vars } = unstable_readConfig({ config: configPath });
    dnsOverrides = await localDnsOverrides(new URL("./.dev.vars", import.meta.url), vars);
  }
  return {
    plugins: [react(), cloudflare(dnsOverrides === undefined ? undefined : {
      config: (config) => ({ vars: { ...config.vars, PERI_DNS_OVERRIDES: dnsOverrides } }),
    })],
    resolve: { dedupe: ["yjs", "lib0"] },
    server: { host: "127.0.0.1" },
  };
});
