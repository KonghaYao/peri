import { describe, expect, test } from "bun:test";
import { acpMcpServers } from "../src/agent/peri-instance";
import { SandboxManager } from "../src/sandbox/manager";
import {
  SandboxManagerError,
  type SandboxBackend,
  type SandboxBackendInstance,
  type SandboxBackendStartOptions,
  type SandboxInstanceState,
  type SandboxManagerOptions,
} from "../src/sandbox/types";

const SANDBOX_ID = `sbx_${"a".repeat(32)}`;
const SECOND_ID = `sbx_${"b".repeat(32)}`;
const TOKEN = "test-token-1234567890";

interface InstanceOverrides {
  id?: string;
  runId?: string;
  mcpUrl?: string;
  token?: string;
  workspacePath?: string;
  state?: SandboxInstanceState;
}

class FakeInstance implements SandboxBackendInstance {
  readonly id: string;
  readonly runId: string;
  readonly mcpUrl: string;
  readonly token: string;
  readonly tokenFile: string | null = null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  stateValue: SandboxInstanceState;
  closeCalls = 0;
  deleteDataCalls: boolean[] = [];
  closeError: unknown;

  constructor(overrides: InstanceOverrides = {}) {
    this.id = overrides.id ?? SANDBOX_ID;
    this.runId = overrides.runId ?? "run_01";
    this.mcpUrl = overrides.mcpUrl ?? `http://127.0.0.1:43116/mcp`;
    this.token = overrides.token ?? TOKEN;
    this.workspacePath = overrides.workspacePath ?? `/tmp/peri-sandbox/sandboxes/${this.id}/workspace`;
    this.sandboxRoot = `/tmp/peri-sandbox/sandboxes/${this.id}`;
    this.stateValue = overrides.state ?? "ready";
  }

  get state(): SandboxInstanceState {
    return this.stateValue;
  }

  async close(options?: { deleteData?: boolean }): Promise<void> {
    this.closeCalls += 1;
    this.deleteDataCalls.push(options?.deleteData === true);
    if (this.closeError) throw this.closeError;
    this.stateValue = "closed";
  }
}

class FakeBackend implements SandboxBackend {
  readonly instances: FakeInstance[] = [];
  readonly startCalls: SandboxBackendStartOptions[] = [];
  readonly deleteCalls: Array<{ sandboxId: string; sandboxesRoot?: string; settingsPath?: string }> = [];
  startError: unknown;
  closeError: unknown;
  deleteError: unknown;
  startGate: Promise<void> | null = null;

  async start(options: SandboxBackendStartOptions): Promise<SandboxBackendInstance> {
    this.startCalls.push(options);
    if (this.startGate) await this.startGate;
    if (this.startError) {
      const error = this.startError;
      this.startError = undefined;
      throw error;
    }
    const index = this.startCalls.length;
    const instance = new FakeInstance({
      id: options.sandboxId ?? SANDBOX_ID,
      runId: `run_${index.toString().padStart(2, "0")}`,
      mcpUrl: `http://127.0.0.1:${43116 + index}/mcp`,
    });
    instance.closeError = this.closeError;
    this.instances.push(instance);
    return instance;
  }

  async deleteData(options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }): Promise<void> {
    this.deleteCalls.push(options);
    if (this.deleteError) throw this.deleteError;
  }
}

function createManager(
  overrides: SandboxManagerOptions = {},
  backend = new FakeBackend(),
): { backend: FakeBackend; manager: SandboxManager } {
  const manager = new SandboxManager({ backend, ...overrides });
  return { backend, manager };
}

async function expectErrorCode(promise: Promise<unknown>, code: string): Promise<SandboxManagerError> {
  let caught: unknown;
  try {
    await promise;
  } catch (error) {
    caught = error;
  }
  expect(caught).toBeInstanceOf(SandboxManagerError);
  const failure = caught as SandboxManagerError;
  expect(failure.code).toBe(code);
  return failure;
}

async function waitFor(check: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (check()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  throw new Error("Timed out waiting for the test condition.");
}

describe("SandboxManager", () => {
  test("manages several handles by sandboxId", async () => {
    const { backend, manager } = createManager();

    expect(manager.size).toBe(0);
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SECOND_ID });

    expect(manager.size).toBe(2);
    expect(manager.get(SANDBOX_ID)).toBe(first);
    expect(manager.get(SECOND_ID)).toBe(second);
    expect(manager.list()).toEqual([first, second]);
    expect(first.state).toBe("ready");
    expect(second.state).toBe("ready");
    expect(backend.startCalls.map((options) => options.sandboxId)).toEqual([SANDBOX_ID, SECOND_ID]);
  });

  test("start is idempotent for the same sandboxId", async () => {
    const { backend, manager } = createManager();

    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SANDBOX_ID });

    expect(second).toBe(first);
    expect(backend.startCalls.length).toBe(1);
    expect(manager.size).toBe(1);
  });

  test("concurrent starts for one id share a single in-flight start", async () => {
    const { backend, manager } = createManager();
    let release: () => void = () => undefined;
    backend.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const first = manager.start({ sandboxId: SANDBOX_ID });
    const second = manager.start({ sandboxId: SANDBOX_ID });
    await waitFor(() => backend.startCalls.length === 1);

    release();
    const [a, b] = await Promise.all([first, second]);
    expect(b).toBe(a);
    expect(backend.startCalls.length).toBe(1);
    expect(manager.size).toBe(1);
  });

  test("different ids start in parallel", async () => {
    const { backend, manager } = createManager();
    let release: () => void = () => undefined;
    backend.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const first = manager.start({ sandboxId: SANDBOX_ID });
    const second = manager.start({ sandboxId: SECOND_ID });
    await waitFor(() => backend.startCalls.length === 2);

    release();
    const [a, b] = await Promise.all([first, second]);
    expect(a.sandboxId).toBe(SANDBOX_ID);
    expect(b.sandboxId).toBe(SECOND_ID);
    expect(manager.size).toBe(2);
  });

  test("generates distinct sbx ids when no sandboxId is given", async () => {
    const { manager } = createManager();

    const first = await manager.start();
    const second = await manager.start();

    expect(first.sandboxId).toMatch(/^sbx_[a-f0-9]{32}$/);
    expect(second.sandboxId).toMatch(/^sbx_[a-f0-9]{32}$/);
    expect(second.sandboxId).not.toBe(first.sandboxId);
    expect(manager.size).toBe(2);
  });

  test("a failed start removes the handle so it can be retried", async () => {
    const { backend, manager } = createManager();
    backend.startError = new Error("backend refused to start");

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "start_failed");
    expect(failure.message).toContain("backend refused to start");
    expect(manager.size).toBe(0);
    expect(manager.get(SANDBOX_ID)).toBeUndefined();

    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    expect(handle.state).toBe("ready");
    expect(backend.startCalls.length).toBe(2);
    expect(manager.size).toBe(1);
  });

  test("chatSessionInit produces the ACP HTTP MCP wire shape", async () => {
    const { manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    const init = await handle.chatSessionInit({ instructions: "Use the sandbox workspace." });

    expect(init.path).toBe(`/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`);
    expect(init.instructions).toBe("Use the sandbox workspace.");
    expect(acpMcpServers(init.mcpServers)).toEqual([
      {
        name: "workspace",
        type: "http",
        url: "http://127.0.0.1:43117/mcp",
        headers: [{ name: "Authorization", value: `Bearer ${TOKEN}` }],
      },
    ]);
  });

  test("custom server names are allowed and reserved names are rejected", async () => {
    const { manager } = createManager({ serverName: "sandbox" });
    await manager.start({ sandboxId: SANDBOX_ID });
    expect((await manager.get(SANDBOX_ID)?.chatSessionInit())?.mcpServers.sandbox).toBeDefined();

    for (const name of ["web", "artifact", "cron", "workflow", " "])
      expect(() => new SandboxManager({ serverName: name })).toThrow(SandboxManagerError);

    await expectErrorCode(manager.start({ serverName: "web" }), "invalid_server_name");
    expect(manager.size).toBe(1);
  });

  test("start forwards manager defaults plus per-sandbox overrides", async () => {
    const { backend, manager } = createManager({
      sandboxesRoot: "/tmp/sandboxes",
      settingsPath: "/tmp/settings.json",
      profileName: "default",
      port: 0,
      periPath: "/usr/local/bin/peri",
      periSha256: "abc123",
      bashCalls: 7,
      minFreeBytes: 1024,
      startupTimeoutMs: 1234,
      serverName: "sandbox",
    });

    await manager.start({ sandboxId: SANDBOX_ID, port: 4567, bashCalls: 9, serverName: "other" });

    expect(backend.startCalls[0]).toMatchObject({
      sandboxId: SANDBOX_ID,
      sandboxesRoot: "/tmp/sandboxes",
      settingsPath: "/tmp/settings.json",
      profileName: "default",
      port: 4567,
      periPath: "/usr/local/bin/peri",
      periSha256: "abc123",
      bashCalls: 9,
      minFreeBytes: 1024,
      startupTimeoutMs: 1234,
    });
    expect(backend.startCalls[0]).not.toHaveProperty("mode");
    expect(backend.startCalls[0]).not.toHaveProperty("controlSocketPath");
    expect(backend.startCalls[0]).not.toHaveProperty("tokenFile");
    expect(backend.startCalls[0]).not.toHaveProperty("configPath");
  });

  test("status and endpoint never contain the token", async () => {
    const { manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    expect(JSON.stringify(handle.status())).not.toContain(TOKEN);
    expect(JSON.stringify(handle.endpoint())).not.toContain(TOKEN);
    expect(handle.status()).toEqual({
      sandboxId: SANDBOX_ID,
      state: "ready",
      mcpUrl: "http://127.0.0.1:43117/mcp",
      workspacePath: `/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`,
      lastError: null,
    });
  });

  test("backend errors containing a known token are redacted and drop the cause", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    backend.instances[0].closeError = Object.assign(new Error(`rejected token ${TOKEN} for Bearer ${TOKEN}`), {
      code: "stop_incomplete",
    });

    const failure = await expectErrorCode(handle.stop(), "stop_incomplete");
    expect(failure.message).not.toContain(TOKEN);
    expect(failure.message).toContain("[redacted]");
    expect(failure.cause).toBeUndefined();
  });

  test("Bearer credentials are redacted even before a token is known", async () => {
    const { backend, manager } = createManager();
    backend.startError = Object.assign(new Error(`authorization failed: Bearer ${TOKEN}`), {
      code: "start_failed",
    });

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "start_failed");
    expect(failure.message).not.toContain(TOKEN);
    expect(failure.message).toContain("Bearer [redacted]");
  });

  test("stop closes the instance but keeps the handle and workspace", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    const first = handle.endpoint();
    expect(first).not.toBeNull();
    await handle.stop();

    expect(backend.instances[0].closeCalls).toBe(1);
    expect(handle.state).toBe("stopped");
    expect(handle.endpoint()).toBeNull();
    expect(manager.get(SANDBOX_ID)).toBe(handle);
    expect(handle.status().sandboxId).toBe(SANDBOX_ID);
    expect(handle.status().workspacePath).toBe(first?.workspacePath);

    const restarted = await manager.start({ sandboxId: SANDBOX_ID });
    expect(restarted).toBe(handle);
    expect(restarted.state).toBe("ready");
    const second = handle.endpoint();
    expect(second?.sandboxId).toBe(SANDBOX_ID);
    expect(second?.workspacePath).toBe(first?.workspacePath);
    expect(second?.runId).not.toBe(first?.runId);
    expect(second?.mcpUrl).not.toBe(first?.mcpUrl);
    expect(backend.startCalls[1].sandboxId).toBe(SANDBOX_ID);
  });

  test("close removes the handle and rejects a later start", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    await handle.close();
    await handle.close();

    expect(backend.instances[0].closeCalls).toBe(1);
    expect(handle.state).toBe("closed");
    expect(manager.size).toBe(0);
    expect(manager.get(SANDBOX_ID)).toBeUndefined();
    await expectErrorCode(handle.start(), "closed");

    const replacement = await manager.start({ sandboxId: SANDBOX_ID });
    expect(replacement).not.toBe(handle);
    expect(manager.size).toBe(1);
  });

  test("close({deleteData:true}) deletes through the instance and supports retry after stop", async () => {
    const first = createManager();
    const handle = await first.manager.start({ sandboxId: SANDBOX_ID });
    await handle.close({ deleteData: true });
    expect(first.backend.instances[0].deleteDataCalls).toEqual([true]);
    expect(first.manager.size).toBe(0);

    const second = createManager();
    const stopped = await second.manager.start({ sandboxId: SANDBOX_ID });
    await stopped.stop();
    await stopped.close({ deleteData: true });
    expect(second.backend.deleteCalls).toEqual([{ sandboxId: SANDBOX_ID }]);
    expect(second.manager.size).toBe(0);
  });

  test("delete failures surface delete_incomplete and keep the handle for retry", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    await handle.stop();
    backend.deleteError = Object.assign(new Error("cannot delete"), { code: "delete_incomplete" });

    await expectErrorCode(handle.close({ deleteData: true }), "delete_incomplete");
    expect(handle.state).toBe("failed");
    expect(manager.get(SANDBOX_ID)).toBe(handle);

    backend.deleteError = undefined;
    await handle.close({ deleteData: true });
    expect(backend.deleteCalls.length).toBe(2);
    expect(handle.state).toBe("closed");
    expect(manager.size).toBe(0);
  });

  test("closeAll closes every handle concurrently and detaches them", async () => {
    const { backend, manager } = createManager();
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SECOND_ID });

    await manager.closeAll();

    expect(first.state).toBe("closed");
    expect(second.state).toBe("closed");
    expect(manager.size).toBe(0);
    expect(backend.instances.map((instance) => instance.closeCalls)).toEqual([1, 1]);
  });

  test("closeAll keeps failed handles and reports an AggregateError that can be retried", async () => {
    const { backend, manager } = createManager();
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SECOND_ID });
    backend.instances[0].closeError = Object.assign(new Error("cannot stop"), { code: "stop_incomplete" });

    let caught: unknown;
    try {
      await manager.closeAll();
    } catch (error) {
      caught = error;
    }
    expect(caught).toBeInstanceOf(AggregateError);
    expect((caught as AggregateError).errors.length).toBe(1);
    expect(manager.size).toBe(1);
    expect(manager.get(SANDBOX_ID)).toBe(first);
    expect(second.state).toBe("closed");

    backend.instances[0].closeError = undefined;
    await manager.closeAll();
    expect(manager.size).toBe(0);
    expect(first.state).toBe("closed");
  });

  test("an unexpected backend exit is reported as backend_exited", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    backend.instances[0].stateValue = "failed";

    expect(handle.state).toBe("failed");
    expect(handle.status().lastError).toEqual({
      code: "backend_exited",
      message: "Sandbox backend exited unexpectedly.",
    });
    expect(handle.endpoint()).toBeNull();

    await handle.close();
    expect(manager.size).toBe(0);
  });

  test("a missing peri-sandbox module reports backend_unavailable", async () => {
    const manager = new SandboxManager({
      moduleSpecifier: `missing-peri-sandbox-${Date.now()}`,
    });
    await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "backend_unavailable");
    expect(manager.size).toBe(0);
  });

  test("a backend id mismatch is rejected as conflict and detaches the handle", async () => {
    const backend = new FakeBackend();
    backend.start = async () => new FakeInstance({ id: SECOND_ID });
    const manager = new SandboxManager({ backend });

    await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "conflict");
    expect(manager.size).toBe(0);
  });

  test("stop failures keep the handle failed and allow a retry", async () => {
    const { backend, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    backend.instances[0].closeError = Object.assign(new Error("cannot stop"), {
      code: "stop_incomplete",
    });

    await expectErrorCode(handle.stop(), "stop_incomplete");
    expect(handle.state).toBe("failed");
    expect(manager.get(SANDBOX_ID)).toBe(handle);

    backend.instances[0].closeError = undefined;
    await handle.stop();
    expect(handle.state).toBe("stopped");
  });
});
