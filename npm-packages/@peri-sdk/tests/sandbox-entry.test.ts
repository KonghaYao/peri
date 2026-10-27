import { expect, test } from "bun:test";
import { resolve } from "node:path";

const dist = resolve(import.meta.dir, "../dist");
const hasDist = await Bun.file(resolve(dist, "index.js")).exists();
const entryTest = hasDist ? test : test.skip;

entryTest("non-sandbox bundles do not reference peri-sandbox, sandbox-runtime or better-sqlite3", async () => {
  for (const file of ["index.js", "portable.js", "wasm.js", "view.js"]) {
    const source = await Bun.file(resolve(dist, file)).text();
    expect(source, `${file} must not reference peri-sandbox`).not.toContain("peri-sandbox");
    expect(source, `${file} must not reference sandbox-runtime`).not.toContain("@anthropic-ai/sandbox-runtime");
    expect(source, `${file} must not reference better-sqlite3`).not.toContain("better-sqlite3");
  }
});

entryTest("sandbox bundle exists and exports the public entry", async () => {
  const bundle = resolve(dist, "sandbox.js");
  expect(await Bun.file(bundle).exists()).toBe(true);

  const module = await import(bundle);
  expect(module.SandboxManager).toBeFunction();
  expect(module.SandboxManagerError).toBeFunction();

  const declaration = resolve(dist, "entries/sandbox.d.ts");
  expect(await Bun.file(declaration).exists()).toBe(true);
});

entryTest("sandbox bundle does not reference a concrete adapter implementation", async () => {
  const source = await Bun.file(resolve(dist, "sandbox.js")).text();
  for (const marker of ["peri-sandbox", "better-sqlite3", "@anthropic-ai/sandbox-runtime"])
    expect(source, `sandbox.js must not reference ${marker}`).not.toContain(marker);
});

entryTest("sandbox bundle does not pull in agent or session modules", async () => {
  const source = await Bun.file(resolve(dist, "sandbox.js")).text();
  for (const marker of ["agent/peri-instance", "agent/session", "contracts/acp", "ChatSessionInit", "AcpMcpServer"])
    expect(source, `sandbox.js must not contain ${marker}`).not.toContain(marker);
});
