import { defineConfig } from "vitest/config";

// The full gate includes real production deadlines; quick excludes only explicitly tagged slow cases.
export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/replay-v2/**/*.test.ts", "tests/replay-v3/**/*.test.ts"],
    testNamePattern: process.env.PERI_E2E_REPLAY_PROFILE === "quick" ? /^(?!.*\[slow\]).*$/ : undefined,
    testTimeout: 240_000,
    hookTimeout: 120_000,
    teardownTimeout: 20_000,
    fileParallelism: process.env.PERI_E2E_REPLAY_MODE !== "record",
    maxWorkers: process.env.PERI_E2E_REPLAY_MODE === "record" ? 1 : 3,
    maxConcurrency: 1,
  },
});
