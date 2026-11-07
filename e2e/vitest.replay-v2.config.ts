import { defineConfig } from "vitest/config";

// A default run only opens sealed fixtures. Recording is an explicit local mode in the harness.
export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/replay-v2/**/*.test.ts"],
    testTimeout: 180_000,
    hookTimeout: 120_000,
    teardownTimeout: 10_000,
    fileParallelism: process.env.PERI_E2E_REPLAY_MODE !== "record",
    maxWorkers: process.env.PERI_E2E_REPLAY_MODE === "record" ? 1 : 3,
    maxConcurrency: 1,
  },
});
