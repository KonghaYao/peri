import { defineConfig } from "vitest/config";
import path from "node:path";

const PROJECT_ROOT = path.resolve(__dirname, "..");

export default defineConfig({
  test: {
    globals: true,
    environment: "node",
    testTimeout: 300_000, // 5 minutes for e2e with real LLM
    hookTimeout: 60_000,
    // Vitest 4: 顺序执行避免 tmux session 冲突
    fileParallelism: false,
    maxConcurrency: 1,
    bail: 0,
    exclude: ["node_modules/**", "tui-tester/**", "dist/**"],
    setupFiles: ["./tests/setup.ts"],
    teardownTimeout: 10_000,
    env: {
      PROJECT_ROOT,
    },
    // 全局 teardown: 测试完成后自动生成 HTML 报告
    globalTeardown: ["./scripts/generate-report.ts"],
  },
});
