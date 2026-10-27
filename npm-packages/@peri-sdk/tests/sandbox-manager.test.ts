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
const MCP_URL = "http://127.0.0.1:43117/mcp";
const SECOND_MCP_URL = "http://127.0.0.1:43118/mcp";
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
    this.runId = overrides.runId ?? "run_one";
    this.mcpUrl = overrides.mcpUrl ?? MCP_URL;
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
  started: Promise<void>;
  private notifyStarted: () => void = () => undefined;

  constructor() {
    this.started = new Promise<void>((resolve) => {
      this.notifyStarted = resolve;
    });
  }

  async start(options: SandboxBackendStartOptions): Promise<SandboxBackendInstance> {
    this.startCalls.push(options);
    this.notifyStarted();
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
      mcpUrl: index === 1 ? MCP_URL : SECOND_MCP_URL,
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

describe("SandboxManager", () => {
  test("start is idempotent and returns the same endpoint once ready", async () => {
    const { backend, manager } = createManager();

    const first = await manager.start();
    const second = await manager.start();

    expect(second).toBe(first);
    expect(backend.startCalls.length).toBe(1);
    expect(manager.state).toBe("ready");
    expect(first.sandboxId).toBe(SANDBOX_ID);
    expect(first.serverName).toBe("workspace");
    expect(first.runId).toBe("run_01");
    expect(first.workspacePath).toBe(`/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`);
    expect(JSON.stringify(first)).not.toContain(TOKEN);
  });

  test("concurrent start calls share one in-flight promise", async () => {
    const { backend, manager } = createManager();
    let release: () => void = () => undefined;
    backend.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const first = manager.start();
    const second = manager.start();
    await backend.started;

    expect(backend.startCalls.length).toBe(1);
    release();
    const [a, b] = await Promise.all([first, second]);
    expect(b).toBe(a);
    expect(manager.state).toBe("ready");
  });

  test("a failed start can be retried", async () => {
    const { backend, manager } = createManager();
    backend.startError = new Error("backend refused to start");

    const failure = await expectErrorCode(manager.start(), "start_failed");
    expect(failure.message).toContain("backend refused to start");
    expect(manager.state).toBe("failed");

    const endpoint = await manager.start();
    expect(endpoint.sandboxId).toBe(SANDBOX_ID);
    expect(backend.startCalls.length).toBe(2);
    expect(manager.state).toBe("ready");
  });

  test("chatSessionInit produces the ACP HTTP MCP wire shape", async () => {
    const { manager } = createManager();
    const init = await manager.chatSessionInit({ instructions: "Use the sandbox workspace." });

    expect(init.path).toBe(`/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`);
    expect(init.instructions).toBe("Use the sandbox workspace.");
    expect(acpMcpServers(init.mcpServers)).toEqual([
      {
        name: "workspace",
        type: "http",
        url: MCP_URL,
        headers: [{ name: "Authorization", value: `Bearer ${TOKEN}` }],
      },
    ]);
  });

  test("custom server names are allowed and reserved names are rejected", async () => {
    const { manager } = createManager({ serverName: "sandbox" });
    expect((await manager.chatSessionInit()).mcpServers.sandbox).toBeDefined();
    for (const name of ["web", "artifact", "cron", "workflow", " "])
      expect(() => new SandboxManager({ serverName: name })).toThrow(SandboxManagerError);
  });

  test("start forwards only single-instance options", async () => {
    const { backend, manager } = createManager({ sessionKey: "chat-1", port: 0 });
    await manager.start();
    expect(backend.startCalls[0]).toMatchObject({ sessionKey: "chat-1", port: 0 });
    expect(backend.startCalls[0]).not.toHaveProperty("mode");
    expect(backend.startCalls[0]).not.toHaveProperty("controlSocketPath");
    expect(backend.startCalls[0]).not.toHaveProperty("tokenFile");
    expect(backend.startCalls[0]).not.toHaveProperty("configPath");
  });

  test("sandboxId and sessionKey are mutually exclusive", () => {
    expect(() => new SandboxManager({ sandboxId: SANDBOX_ID, sessionKey: "chat-1" })).toThrow(SandboxManagerError);
  });

  test("status and endpoint never contain the token", async () => {
    const { manager } = createManager();
    await manager.start();
    expect(JSON.stringify(manager.status())).not.toContain(TOKEN);
    expect(JSON.stringify(manager.endpoint())).not.toContain(TOKEN);
    expect(manager.status().sandboxState).toBe("ready");
    expect(manager.status().sandboxId).toBe(SANDBOX_ID);
    expect(manager.status().mcpUrl).toBe(MCP_URL);
    expect(manager.status().lastError).toBeNull();
  });

  test("backend errors containing a known token are redacted and drop the cause", async () => {
    const { backend, manager } = createManager();
    await manager.start();
    backend.instances[0].closeError = Object.assign(new Error(`rejected token ${TOKEN} for Bearer ${TOKEN}`), {
      code: "stop_incomplete",
    });
    const failure = await expectErrorCode(manager.stop(), "stop_incomplete");
    expect(failure.message).not.toContain(TOKEN);
    expect(failure.message).toContain("[redacted]");
    expect(failure.cause).toBeUndefined();
  });

  test("Bearer credentials are redacted even before a token is known", async () => {
    const { backend, manager } = createManager();
    backend.startError = Object.assign(new Error(`authorization failed: Bearer ${TOKEN}`), {
      code: "start_failed",
    });
    const failure = await expectErrorCode(manager.start(), "start_failed");
    expect(failure.message).not.toContain(TOKEN);
    expect(failure.message).toContain("Bearer [redacted]");
  });

  test("stop closes the instance but keeps the sandbox id and workspace", async () => {
    const { backend, manager } = createManager();
    const first = await manager.start();
    await manager.stop();

    expect(backend.instances[0].closeCalls).toBe(1);
    expect(manager.state).toBe("stopped");
    expect(manager.endpoint()).toBeNull();
    expect(manager.status().sandboxId).toBe(SANDBOX_ID);
    expect(manager.status().workspacePath).toBe(first.workspacePath);

    const second = await manager.start();
    expect(second.sandboxId).toBe(SANDBOX_ID);
    expect(second.workspacePath).toBe(first.workspacePath);
    expect(second.runId).not.toBe(first.runId);
    expect(second.mcpUrl).not.toBe(first.mcpUrl);
    expect(backend.startCalls[1].sandboxId).toBe(SANDBOX_ID);
  });

  test("close is idempotent and keeps data unless deleteData is requested", async () => {
    const { backend, manager } = createManager();
    await manager.start();
    await manager.close();
    await manager.close();

    expect(backend.instances[0].closeCalls).toBe(1);
    expect(backend.deleteCalls).toEqual([]);
    expect(manager.state).toBe("closed");
    expect(await expectErrorCode(manager.start(), "closed")).toBeInstanceOf(SandboxManagerError);
  });

  test("close({deleteData:true}) deletes through the instance and supports retry after stop", async () => {
    const first = createManager();
    await first.manager.start();
    await first.manager.close({ deleteData: true });
    expect(first.backend.instances[0].deleteDataCalls).toEqual([true]);
    expect(first.manager.state).toBe("closed");

    const second = createManager();
    await second.manager.start();
    await second.manager.stop();
    await second.manager.close({ deleteData: true });
    expect(second.backend.deleteCalls).toEqual([{ sandboxId: SANDBOX_ID }]);
    expect(second.manager.state).toBe("closed");
  });

  test("delete failures surface delete_incomplete and can be retried", async () => {
    const { backend, manager } = createManager();
    await manager.start();
    await manager.stop();
    backend.deleteError = Object.assign(new Error("cannot delete"), { code: "delete_incomplete" });
    await expectErrorCode(manager.close({ deleteData: true }), "delete_incomplete");
    expect(manager.state).toBe("failed");
    backend.deleteError = undefined;
    await manager.close({ deleteData: true });
    expect(backend.deleteCalls.length).toBe(2);
    expect(manager.state).toBe("closed");
  });

  test("an unexpected backend exit is reported as backend_exited", async () => {
    const { backend, manager } = createManager();
    await manager.start();
    backend.instances[0].stateValue = "failed";

    const status = manager.status();
    expect(manager.state).toBe("failed");
    expect(status.lastError).toEqual({
      code: "backend_exited",
      message: "Sandbox backend exited unexpectedly.",
    });
    expect(manager.endpoint()).toBeNull();
    await manager.close();
    expect(manager.state).toBe("closed");
  });

  test("a missing peri-sandbox module reports backend_unavailable", async () => {
    const manager = new SandboxManager({
      moduleSpecifier: `missing-peri-sandbox-${Date.now()}`,
    });
    await expectErrorCode(manager.start(), "backend_unavailable");
    await manager.close();
  });

  test("stop failures keep the manager failed and allow a retry", async () => {
    const { backend, manager } = createManager();
    await manager.start();
    backend.instances[0].closeError = Object.assign(new Error("cannot stop"), {
      code: "stop_incomplete",
    });
    await expectErrorCode(manager.stop(), "stop_incomplete");
    expect(manager.state).toBe("failed");
    backend.instances[0].closeError = undefined;
    await manager.stop();
    expect(manager.state).toBe("stopped");
  });
});
