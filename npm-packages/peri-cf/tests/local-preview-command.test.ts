import { expect, test } from "bun:test";
import { resolve } from "node:path";
import { localPreviewCommand } from "../scripts/local-preview-command";

test("compiled preview runs the installed Wrangler CLI rather than Vite or the preview script recursively", () => {
  const root = resolve("/tmp/peri compiled worker");
  expect(localPreviewCommand(root)).toEqual([
    "node", resolve(root, "node_modules/wrangler/bin/wrangler.js"), "dev", "--config",
    resolve(root, "dist/peri_cf/wrangler.json"), "--ip", "127.0.0.1", "--port", "8791",
  ]);
});

test("additional CLI arguments are forwarded without shell interpolation", () => {
  const root = resolve("/tmp/peri compiled worker");
  const extra = ["--persist-to", "/tmp/peri state;not-a-shell-command", "--log-level", "debug"];
  const command = localPreviewCommand(root, extra);
  expect(command.slice(-4)).toEqual(extra);
  expect(command[4]).toBe(resolve(root, "dist/peri_cf/wrangler.json"));
  expect(extra).toEqual(["--persist-to", "/tmp/peri state;not-a-shell-command", "--log-level", "debug"]);
});
