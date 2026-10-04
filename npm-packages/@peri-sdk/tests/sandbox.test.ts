import { expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Sandbox } from "../src/sandbox/sandbox.ts";

test("external Workspace keeps its remote path identity despite a local symlink", async () => {
  const root = mkdtempSync(join(tmpdir(), "peri-sdk-sandbox-"));
  try {
    const actual = join(root, "actual");
    const remotePath = join(root, "remote-alias");
    mkdirSync(actual);
    symlinkSync(actual, remotePath, "dir");
    const queries: string[] = [];
    const external = new Sandbox({
      id: "external",
      path: remotePath,
      workspace: { url: "http://workspace.example" },
      storage: {
        deployment: () => ({ args: [], env: {} }),
        getSessions: async (cwd) => { queries.push(cwd); return []; },
      },
    });

    expect(external.path).toBe(remotePath);
    await external.getSessions();
    expect(queries).toEqual([remotePath]);

    const managed = new Sandbox({ id: "managed", path: remotePath });
    expect(managed.path).toBe(realpathSync(actual));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("external Workspace accepts a session path absent from the SDK host", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-sdk-sandbox-"));
  try {
    const remotePath = join(root, "remote-only");
    const external = new Sandbox({
      id: "external",
      path: remotePath,
      workspace: { url: "http://workspace.example" },
      stdio: { cwd: root },
    });
    expect(external.path).toBe(remotePath);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
