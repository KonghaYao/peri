import { expect, test } from "bun:test";
import { createPeriSandboxAdapter, type PeriSandboxStartOptions } from "peri-sandbox";
import type { SandboxAdapter } from "../src/sandbox";

/**
 * Compile-time compatibility check: `peri-sandbox` implements the SDK adapter
 * contract structurally without importing SDK types. No sandbox is started.
 */
test("peri-sandbox adapter satisfies the SDK SandboxAdapter contract", () => {
  const adapter: SandboxAdapter<PeriSandboxStartOptions> = createPeriSandboxAdapter({
    settingsPath: "/tmp/peri-sandbox/settings.json",
    sandboxesRoot: "/tmp/peri-sandbox/sandboxes",
  });

  expect(typeof adapter.start).toBe("function");
  expect(typeof adapter.deleteData).toBe("function");
});
