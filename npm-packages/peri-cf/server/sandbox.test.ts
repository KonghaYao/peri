import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import type { WorkspaceRef } from "@peri-code/sdk";
import type { SandboxMcpServer, SandboxStatus } from "@peri-code/sdk/sandbox";
import {
  SandboxPool,
  SandboxStartError,
  listSandboxWorkspaces,
  parseSandboxWorkspace,
} from "./sandbox";

const ID = "sbx_0123456789abcdef0123456789abcdef";
const ROOT = "/sandboxes";

class FakeHandle {
  readonly workspace: WorkspaceRef;
  readonly serverName = "sandbox";
  closeCalls = 0;
  failClose: unknown;
  failMcp: unknown;

  constructor(readonly sandboxId: string) {
    const path = `${ROOT}/${sandboxId}/workspace`;
    this.workspace = { id: sandboxId, hostPath: path, runtimePath: path };
  }

  status(): SandboxStatus {
    return {
      sandboxId: this.sandboxId,
      state: "ready",
      mcpUrl: `http://127.0.0.1/${this.sandboxId}`,
      workspace: this.workspace,
      lastError: null,
    };
  }

  async mcpServer(): Promise<SandboxMcpServer> {
    if (this.failMcp !== undefined) throw this.failMcp;
    return {
      name: "workspace",
      config: { type: "http", url: `http://127.0.0.1/${this.sandboxId}`, headers: {} },
    };
  }

  async close(): Promise<void> {
    this.closeCalls += 1;
    if (this.failClose !== undefined) throw this.failClose;
  }
}

class FakeBackend {
  readonly starts: Array<string | undefined> = [];
  readonly handles = new Map<string, FakeHandle>();
  failStart: unknown;
  private created = 0;

  async start(options: { sandboxId?: string }): Promise<FakeHandle> {
    this.starts.push(options.sandboxId);
    if (this.failStart !== undefined) throw this.failStart;
    const id = options.sandboxId ?? `sbx_${String(++this.created).padStart(32, "0")}`;
    const existing = this.handles.get(id);
    if (existing) return existing;
    const handle = new FakeHandle(id);
    this.handles.set(id, handle);
    return handle;
  }
}

describe("parseSandboxWorkspace", () => {
  test("接受工作区根与子目录，并返回相对 sessionPath", () => {
    expect(parseSandboxWorkspace(ROOT, `${ROOT}/${ID}/workspace`))
      .toEqual({ sandboxId: ID });
    expect(parseSandboxWorkspace(ROOT, `${ROOT}/${ID}/workspace/project`))
      .toEqual({ sandboxId: ID, sessionPath: "project" });
  });

  test("拒绝根目录、越界路径与非法布局", () => {
    expect(parseSandboxWorkspace(ROOT, ROOT)).toBeNull();
    expect(parseSandboxWorkspace(ROOT, `${ROOT}/../escape`)).toBeNull();
    expect(parseSandboxWorkspace(ROOT, `${ROOT}/sbx_short/workspace`)).toBeNull();
    expect(parseSandboxWorkspace(ROOT, `${ROOT}/${ID}/elsewhere`)).toBeNull();
    expect(parseSandboxWorkspace("relative", `${ROOT}/${ID}/workspace`)).toBeNull();
  });
});

describe("listSandboxWorkspaces", () => {
  test("只列出合法 sandbox 目录，缺失根返回空", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-sandbox-list-"));
    try {
      mkdirSync(join(root, ID, "workspace"), { recursive: true });
      mkdirSync(join(root, "sbx_ffffffffffffffffffffffffffffffff", "workspace"), { recursive: true });
      mkdirSync(join(root, "unrelated"), { recursive: true });

      expect(await listSandboxWorkspaces(root)).toEqual([
        join(root, ID, "workspace"),
        join(root, "sbx_ffffffffffffffffffffffffffffffff", "workspace"),
      ]);
      expect(await listSandboxWorkspaces(join(root, "missing"))).toEqual([]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("SandboxPool", () => {
  test("同一 sandboxId 的引用共享实例，归零才关闭", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);

    const first = await pool.acquire(ID);
    const second = await pool.acquire(ID);

    expect(backend.starts).toEqual([ID]);
    expect(first.sandboxId).toBe(ID);
    expect(second.mcp).toEqual(await backend.handles.get(ID)!.mcpServer());

    await first.release();
    expect(backend.handles.get(ID)!.closeCalls).toBe(0);
    await second.release();
    expect(backend.handles.get(ID)!.closeCalls).toBe(1);
    await second.release();
    expect(backend.handles.get(ID)!.closeCalls).toBe(1);
  });

  test("同一 sandboxId 的并发 acquire 只启动一次", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);

    const leases = await Promise.all([pool.acquire(ID), pool.acquire(ID), pool.acquire(ID)]);

    expect(backend.starts).toEqual([ID]);
    await leases[0]!.release();
    expect(backend.handles.get(ID)!.closeCalls).toBe(0);
    await leases[1]!.release();
    await leases[2]!.release();
    expect(backend.handles.get(ID)!.closeCalls).toBe(1);
  });

  test("新会话由 manager 生成 sandboxId", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);

    const lease = await pool.acquire();

    expect(backend.starts).toEqual([undefined]);
    expect(lease.sandboxId).toMatch(/^sbx_[0-9a-f]{32}$/);
    expect(lease.workspace.runtimePath).toBe(`${ROOT}/${lease.sandboxId}/workspace`);
  });

  test("启动失败与 MCP 投影失败都收敛为 SandboxStartError", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);
    backend.failStart = new Error("spawn failed");
    await expect(pool.acquire(ID)).rejects.toBeInstanceOf(SandboxStartError);

    backend.failStart = undefined;
    const handle = new FakeHandle(ID);
    handle.failMcp = new Error("not ready");
    backend.handles.set(ID, handle);
    await expect(pool.acquire(ID)).rejects.toBeInstanceOf(SandboxStartError);
    expect(handle.closeCalls).toBe(1);
  });

  test("释放失败保留实例，重试后关闭", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);
    const lease = await pool.acquire(ID);
    const handle = backend.handles.get(ID)!;
    handle.failClose = new Error("close failed");

    await expect(lease.release()).rejects.toThrow("close failed");
    expect(handle.closeCalls).toBe(1);

    handle.failClose = undefined;
    await lease.release();
    expect(handle.closeCalls).toBe(2);
  });

  test("snapshot 只读列举活动实例、引用数与启动中计数", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);
    const other = "sbx_ffffffffffffffffffffffffffffffff";
    await pool.acquire(other);
    await pool.acquire(ID);
    await pool.acquire(ID);

    const snapshot = pool.snapshot();
    expect(backend.starts).toEqual([other, ID]);
    expect(snapshot.starting).toBe(0);
    expect(snapshot.active).toEqual([
      {
        sandboxId: ID,
        state: "ready",
        refs: 2,
        startedAtMs: expect.any(Number),
        workspacePath: `${ROOT}/${ID}/workspace`,
        mcpUrl: `http://127.0.0.1/${ID}`,
        mcpServerName: "sandbox",
        lastError: null,
      },
      {
        sandboxId: other,
        state: "ready",
        refs: 1,
        startedAtMs: expect.any(Number),
        workspacePath: `${ROOT}/${other}/workspace`,
        mcpUrl: `http://127.0.0.1/${other}`,
        mcpServerName: "sandbox",
        lastError: null,
      },
    ]);
    expect(backend.starts).toEqual([other, ID]);
  });

  test("缺少 status 的假后端按 unknown 报告，不触发启动", async () => {
    const workspace: WorkspaceRef = {
      id: ID,
      hostPath: `${ROOT}/${ID}/workspace`,
      runtimePath: `${ROOT}/${ID}/workspace`,
    };
    let starts = 0;
    const pool = new SandboxPool({
      async start() {
        starts += 1;
        return {
          sandboxId: ID,
          workspace,
          mcpServer: async () => ({
            name: "workspace",
            config: { type: "http", url: "http://127.0.0.1/mcp", headers: {} },
          }),
          close: async () => {},
        };
      },
    });
    await pool.acquire(ID);

    expect(pool.snapshot().active).toEqual([expect.objectContaining({
      sandboxId: ID,
      state: "unknown",
      mcpUrl: null,
      mcpServerName: null,
      lastError: null,
    })]);
    expect(starts).toBe(1);
  });

  test("closeAll 关闭全部实例并聚合失败", async () => {
    const backend = new FakeBackend();
    const pool = new SandboxPool(backend);
    await pool.acquire(ID);
    await pool.acquire("sbx_ffffffffffffffffffffffffffffffff");
    const failing = backend.handles.get(ID)!;
    failing.failClose = new Error("close failed");

    await expect(pool.closeAll()).rejects.toBeInstanceOf(AggregateError);
    expect(failing.closeCalls).toBe(1);
    expect(backend.handles.get("sbx_ffffffffffffffffffffffffffffffff")!.closeCalls).toBe(1);
  });
});
