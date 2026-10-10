import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import { PROJECT_ROOT } from "./peri.js";

export async function buildPeriForE2e(): Promise<void> {
  await promisify(execFile)("bash", [
    path.join(PROJECT_ROOT, "scripts/cargo-rmcp-patched.sh"),
    "build", "--locked", "-p", "peri-tui", "--bin", "peri",
  ], {
    cwd: PROJECT_ROOT,
    timeout: 600_000,
    maxBuffer: 8 * 1024 * 1024,
  });
}
