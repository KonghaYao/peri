#!/usr/bin/env node
/**
 * E2E 分层门禁入口（供 CI / release-prep / 本地发版脚本调用）。
 *
 * 用法（在 e2e/ 目录）:
 *   node scripts/e2e-gate.mjs l0
 *   node scripts/e2e-gate.mjs l1
 *   node scripts/e2e-gate.mjs release
 *   node scripts/e2e-gate.mjs release --strict   # 首轮必须全绿（无 flake 预算）
 */
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const RUN = path.join(__dirname, "run-e2e.mjs");

const tier = process.argv[2];
const strict = process.argv.includes("--strict");

if (!tier || tier === "-h" || tier === "--help") {
  console.log(`用法: node scripts/e2e-gate.mjs <l0|l1|l2|release> [--strict]

  l0       PR 冒烟（串行，无 retry）
  l1       合并前 panels + tool-cards + smoke
  release  发版全量（默认允许首轮 ≤2 flake，retry 后须全绿）
  --strict 首轮必须全绿（等价 --require-clean-first-pass）`);
  process.exit(tier ? 0 : 2);
}

const args = ["--tier", tier, "--no-interactive"];
if (strict) {
  args.push("--require-clean-first-pass");
}

const result = spawnSync(process.execPath, [RUN, ...args], {
  stdio: "inherit",
  cwd: path.resolve(__dirname, ".."),
});

process.exit(result.status ?? 1);
