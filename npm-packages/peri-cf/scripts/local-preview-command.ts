import { resolve } from "node:path";

export function localPreviewCommand(packageRoot: string, arguments_: readonly string[] = []): string[] {
  return ["node", resolve(packageRoot, "node_modules/wrangler/bin/wrangler.js"),
    "dev", "--config", resolve(packageRoot, "dist/peri_cf/wrangler.json"),
    "--ip", "127.0.0.1", "--port", "8791", ...arguments_];
}
