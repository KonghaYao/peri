import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { workerGlue } from "../scripts/worker-glue";

describe("Workers platform WASM glue", () => {
  test("adapts real SDK artifacts without modifying the SDK source", async () => {
    const source = await readFile(new URL("../../@peri-sdk/dist/wasm/peri-wasm.js", import.meta.url), "utf8");
    const result = workerGlue(source);
    for (const parameter of ["periDns", "periNet", "periWorkerSockets", "periSetImmediate", "periClearImmediate"])
      expect(result).toContain(`Module["${parameter}"]`);
    expect(source).not.toContain('Module["periDns"]');
    expect(() => workerGlue(result)).toThrow("Unsupported SDK WASM glue");
  });

  test("rejects unrecognized artifacts rather than silently leaving a broken runtime", () => {
    expect(() => workerGlue("export default function Module() {}"))
      .toThrow("Unsupported SDK WASM glue");
  });
});
