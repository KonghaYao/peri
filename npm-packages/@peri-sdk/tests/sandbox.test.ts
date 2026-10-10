import { expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Sandbox } from "../src/sandbox/sandbox.ts";
import type { Transport } from "../src/transport/types.ts";

const unusedTransport: Transport = {
  request: async () => { throw new Error("transport must not be used"); },
  sendRequest: async () => { throw new Error("transport must not be used"); },
  notify: async () => {},
  subscribe: () => () => {},
  events: async function* () {},
  setRequestHandler: () => {},
  close: async () => {},
};

test("Sandbox keeps the caller's path verbatim without resolving local symlinks", async () => {
  const root = mkdtempSync(join(tmpdir(), "peri-sdk-sandbox-"));
  try {
    const actual = join(root, "actual");
    const alias = join(root, "alias");
    mkdirSync(actual);
    symlinkSync(actual, alias, "dir");
    const queries: string[] = [];
    const sandbox = new Sandbox({
      id: "external",
      path: alias,
      workspace: { url: "http://workspace.example" },
      storage: { getSessions: async (cwd) => { queries.push(cwd); return []; } },
      transportFactory: () => unusedTransport,
    });

    // 路径属于远端工具环境：本机是否可解析、是否指向同一目录都不改变身份。
    expect(sandbox.path).toBe(alias);
    expect(sandbox.path).not.toBe(realpathSync(actual));
    await sandbox.getSessions();
    expect(queries).toEqual([alias]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Sandbox accepts a session path absent from the SDK host", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-sdk-sandbox-"));
  try {
    const remotePath = join(root, "remote-only");
    const sandbox = new Sandbox({
      id: "external",
      path: remotePath,
      workspace: { url: "http://workspace.example" },
      transportFactory: () => unusedTransport,
    });
    expect(sandbox.path).toBe(remotePath);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Sandbox requires an explicit transport factory instead of starting a process", () => {
  expect(() => new Sandbox({ id: "workspace" } as unknown as ConstructorParameters<typeof Sandbox>[0]))
    .toThrow("Sandbox transportFactory is required");
  expect(() => new Sandbox({ id: "", transportFactory: () => unusedTransport })).toThrow("Sandbox id is required");
});

test("Sandbox without a configured Workspace reports it instead of inventing an endpoint", () => {
  const sandbox = new Sandbox({ id: "workspace", transportFactory: () => unusedTransport });
  expect(sandbox.optionalWorkspace).toBeUndefined();
  expect(() => sandbox.getWorkspace()).toThrow("Sandbox has no HTTP Workspace configured");
  expect(() => sandbox.getSessions()).toThrow("Sandbox has no Session Storage configured");
  expect(() => new Sandbox({ id: "workspace", storage: { getSessions: async () => [], getSession: async () => null },
    transportFactory: () => unusedTransport }).getSessions()).toThrow("Sandbox session path is required");
});
