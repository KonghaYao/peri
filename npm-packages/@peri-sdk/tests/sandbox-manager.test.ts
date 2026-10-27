import { describe, expect, test } from "bun:test";
import { SandboxHandle } from "../src/sandbox/handle";
import { SandboxManager } from "../src/sandbox/manager";
import {
  SandboxManagerError,
  type SandboxAdapter,
  type SandboxAdapterInstance,
  type SandboxAdapterStartOptions,
  type SandboxManagerOptions,
} from "../src/sandbox/types";

const SANDBOX_ID = `sbx_${"a".repeat(32)}`;
const SECOND_ID = `sbx_${"b".repeat(32)}`;
const TOKEN = "test-token-1234567890";

interface FakeOptions {
  profileName?: string;
  port?: number;
  nested?: { a?: number; b?: number };
}

class FakeInstance implements SandboxAdapterInstance {
  readonly sandboxId: string;
  readonly workspacePath: string;
  readonly mcpServer: { type: "http"; url: string; headers: Record<string, string> };
  closeCalls = 0;
  closeDelayMs = 0;
  closeGate: Promise<void> | null = null;
  closeError: unknown;

  constructor(sandboxId: string, url: string) {
    this.sandboxId = sandboxId;
    this.workspacePath = `/tmp/peri-sandbox/sandboxes/${sandboxId}/workspace`;
    this.mcpServer = { type: "http", url, headers: { Authorization: `Bearer ${TOKEN}` } };
  }

  async close(): Promise<void> {
    this.closeCalls += 1;
    if (this.closeGate) await this.closeGate;
    if (this.closeDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, this.closeDelayMs));
    if (this.closeError) throw this.closeError;
  }
}

class FakeAdapter implements SandboxAdapter<FakeOptions> {
  readonly starts: SandboxAdapterStartOptions<FakeOptions>[] = [];
  readonly instances: FakeInstance[] = [];
  readonly deleteCalls: string[] = [];
  startError: unknown;
  deleteError: unknown;
  startGate: Promise<void> | null = null;
  instanceId: string | null = null;

  async start(options: SandboxAdapterStartOptions<FakeOptions>): Promise<SandboxAdapterInstance> {
    this.starts.push(options);
    if (this.startGate) await this.startGate;
    if (this.startError) {
      const error = this.startError;
      this.startError = undefined;
      throw error;
    }
    const index = this.starts.length;
    const sandboxId = this.instanceId ?? options.sandboxId;
    const instance = new FakeInstance(sandboxId, `http://127.0.0.1:${43116 + index}/mcp`);
    this.instances.push(instance);
    return instance;
  }

  async deleteData(sandboxId: string): Promise<void> {
    this.deleteCalls.push(sandboxId);
    if (this.deleteError) {
      const error = this.deleteError;
      this.deleteError = undefined;
      throw error;
    }
  }
}

function createManager(
  overrides: Partial<SandboxManagerOptions<FakeOptions>> = {},
  adapter = new FakeAdapter(),
): { adapter: FakeAdapter; manager: SandboxManager<FakeOptions> } {
  const manager = new SandboxManager<FakeOptions>({ adapter, ...overrides });
  return { adapter, manager };
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
  test("requires an adapter at construction", () => {
    expect(() => new SandboxManager({} as unknown as SandboxManagerOptions)).toThrow(SandboxManagerError);
    try {
      new SandboxManager({} as unknown as SandboxManagerOptions);
    } catch (error) {
      expect((error as SandboxManagerError).code).toBe("invalid_options");
      expect((error as Error).message).toContain("SandboxAdapter");
    }
  });

  test("manages several handles by sandboxId", async () => {
    const { adapter, manager } = createManager();

    expect(manager.size).toBe(0);
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SECOND_ID });

    expect(manager.size).toBe(2);
    expect(manager.get(SANDBOX_ID)).toBe(first);
    expect(manager.get(SECOND_ID)).toBe(second);
    expect(manager.list()).toEqual([first, second]);
    expect(first).toBeInstanceOf(SandboxHandle);
    expect(first.state).toBe("ready");
    expect(second.state).toBe("ready");
    expect(adapter.starts.map((options) => options.sandboxId)).toEqual([SANDBOX_ID, SECOND_ID]);
  });

  test("start is idempotent for the same sandboxId", async () => {
    const { adapter, manager } = createManager();

    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SANDBOX_ID });

    expect(second).toBe(first);
    expect(adapter.starts.length).toBe(1);
    expect(manager.size).toBe(1);
  });

  test("concurrent starts for one id share a single in-flight start", async () => {
    const { adapter, manager } = createManager();
    let release: () => void = () => undefined;
    adapter.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const first = manager.start({ sandboxId: SANDBOX_ID });
    const second = manager.start({ sandboxId: SANDBOX_ID });
    await waitFor(() => adapter.starts.length === 1);

    release();
    const [a, b] = await Promise.all([first, second]);
    expect(b).toBe(a);
    expect(adapter.starts.length).toBe(1);
    expect(manager.size).toBe(1);
  });

  test("different ids start in parallel", async () => {
    const { adapter, manager } = createManager();
    let release: () => void = () => undefined;
    adapter.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const first = manager.start({ sandboxId: SANDBOX_ID });
    const second = manager.start({ sandboxId: SECOND_ID });
    await waitFor(() => adapter.starts.length === 2);

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
    const { adapter, manager } = createManager();
    adapter.startError = new Error("adapter refused to start");

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "start_failed");
    expect(failure.message).toContain("adapter refused to start");
    expect(manager.size).toBe(0);
    expect(manager.get(SANDBOX_ID)).toBeUndefined();

    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    expect(handle.state).toBe("ready");
    expect(adapter.starts.length).toBe(2);
    expect(manager.size).toBe(1);
  });

  test("mcpServer returns the sandbox HTTP MCP wire shape", async () => {
    const { manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    const mcp = await handle.mcpServer();

    expect(mcp).toEqual({
      name: "workspace",
      config: {
        type: "http",
        url: "http://127.0.0.1:43117/mcp",
        headers: { Authorization: `Bearer ${TOKEN}` },
      },
    });
    expect(handle.workspacePath).toBe(`/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`);
  });

  test("mcpServer requires a ready handle and reports not_ready or closed", async () => {
    const { adapter, manager } = createManager();
    let release: () => void = () => undefined;
    adapter.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const pending = manager.start({ sandboxId: SANDBOX_ID });
    await waitFor(() => manager.get(SANDBOX_ID) !== undefined);
    const handle = manager.get(SANDBOX_ID)!;
    expect(handle.state).toBe("starting");
    await expectErrorCode(handle.mcpServer(), "not_ready");

    release();
    await pending;
    expect(handle.state).toBe("ready");
    expect((await handle.mcpServer()).config.headers.Authorization).toBe(`Bearer ${TOKEN}`);

    await handle.close();
    await expectErrorCode(handle.mcpServer(), "closed");
  });

  test("custom server names are allowed and reserved names are rejected", async () => {
    const { manager } = createManager({ serverName: "sandbox" });
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    expect(handle.serverName).toBe("sandbox");
    expect((await handle.mcpServer()).name).toBe("sandbox");

    expect(() => createManager({ serverName: "web" })).toThrow(SandboxManagerError);
    const failure = await expectErrorCode(manager.start({ sandboxId: SECOND_ID, serverName: "cron" }), "invalid_server_name");
    expect(failure.message).toContain("reserved");
  });

  test("forwards manager adapterOptions to the adapter", async () => {
    const options: FakeOptions = { profileName: "manager", nested: { a: 1 } };
    const { adapter, manager } = createManager({ adapterOptions: options });

    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    expect(adapter.starts).toEqual([
      { sandboxId: SANDBOX_ID, options: { profileName: "manager", nested: { a: 1 } } },
    ]);
    expect(handle.sandboxId).toBe(SANDBOX_ID);
  });

  test("per-start adapterOptions replaces the manager default", async () => {
    const { adapter, manager } = createManager({ adapterOptions: { profileName: "manager", port: 1 } });

    await manager.start({ sandboxId: SANDBOX_ID, adapterOptions: { profileName: "start" } });
    await manager.start({ sandboxId: SECOND_ID });

    expect(adapter.starts[0]?.options).toEqual({ profileName: "start" });
    expect(adapter.starts[1]?.options).toEqual({ profileName: "manager", port: 1 });
  });

  test("per-start signal reaches the adapter and is not retained", async () => {
    const { adapter, manager } = createManager();
    const controller = new AbortController();

    await manager.start({ sandboxId: SANDBOX_ID, signal: controller.signal });
    expect(adapter.starts[0]?.signal).toBe(controller.signal);

    await manager.start({ sandboxId: SECOND_ID });
    expect(adapter.starts[1]?.signal).toBeUndefined();
  });

  test("manager signal is the default for starts and does not participate in conflict checks", async () => {
    const managerSignal = new AbortController();
    const { adapter, manager } = createManager({ signal: managerSignal.signal });

    await manager.start({ sandboxId: SANDBOX_ID });
    expect(adapter.starts[0]?.signal).toBe(managerSignal.signal);

    const other = new AbortController();
    const handle = await manager.start({ sandboxId: SANDBOX_ID, signal: other.signal });
    expect(handle.state).toBe("ready");
    expect(adapter.starts.length).toBe(1);
  });

  test("per-start startup timeout bounds a slow adapter", async () => {
    const { adapter, manager } = createManager({ startupTimeoutMs: 5_000 });
    let release: () => void = () => undefined;
    adapter.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const failure = await expectErrorCode(
      manager.start({ sandboxId: SANDBOX_ID, startupTimeoutMs: 5 }),
      "start_failed",
    );
    expect(failure.message).toContain("timed out");
    expect(manager.size).toBe(0);

    release();
    await waitFor(() => adapter.instances.length === 1);
    expect(adapter.instances[0]?.closeCalls).toBe(1);
  });

  test("start reuses an existing handle when explicit overrides match", async () => {
    const { adapter, manager } = createManager({
      adapterOptions: { profileName: "p", nested: { a: 1, b: 2 } },
      startupTimeoutMs: 10_000,
      stopTimeoutMs: 10_000,
      serverName: "sandbox",
    });

    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({
      sandboxId: SANDBOX_ID,
      serverName: "sandbox",
      startupTimeoutMs: 10_000,
      stopTimeoutMs: 10_000,
      adapterOptions: { nested: { b: 2, a: 1 }, profileName: "p" },
    });

    expect(second).toBe(first);
    expect(adapter.starts.length).toBe(1);
  });

  test("omitted fields inherit the handle values when only sandboxId is passed", async () => {
    const { adapter, manager } = createManager({
      adapterOptions: { profileName: "p" },
      serverName: "sandbox",
      startupTimeoutMs: 10_000,
    });

    await manager.start({ sandboxId: SANDBOX_ID });
    await manager.start({ sandboxId: SANDBOX_ID });
    expect(adapter.starts.length).toBe(1);
    expect(adapter.starts[0]?.options).toEqual({ profileName: "p" });
  });

  test("start rejects conflicting options for an existing sandboxId", async () => {
    const { manager } = createManager({
      adapterOptions: { profileName: "p" },
      serverName: "sandbox",
      startupTimeoutMs: 10_000,
      stopTimeoutMs: 10_000,
    });
    await manager.start({ sandboxId: SANDBOX_ID });

    expect((await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID, serverName: "other" }), "conflict")).message).toContain(
      "serverName",
    );
    expect(
      (await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID, startupTimeoutMs: 20_000 }), "conflict")).message,
    ).toContain("startupTimeoutMs");
    expect(
      (await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID, stopTimeoutMs: 20_000 }), "conflict")).message,
    ).toContain("stopTimeoutMs");
    expect(
      (await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID, adapterOptions: { profileName: "q" } }), "conflict"))
        .message,
    ).toContain("adapterOptions");
  });

  test("non-serializable adapterOptions are never silently overwritten", async () => {
    const first = { run: () => 1 };
    const second = { run: () => 2 };
    const manager = new SandboxManager({ adapter: new FakeAdapter(), adapterOptions: first });

    await manager.start({ sandboxId: SANDBOX_ID });
    await manager.start({ sandboxId: SANDBOX_ID, adapterOptions: first });
    await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID, adapterOptions: second }), "conflict");
  });

  test("status and endpoint never contain the token", async () => {
    const { manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    expect(JSON.stringify(handle.status())).not.toContain(TOKEN);
    expect(JSON.stringify(handle.endpoint())).not.toContain(TOKEN);
    expect(handle.endpoint()).toEqual({
      sandboxId: SANDBOX_ID,
      serverName: "workspace",
      mcpUrl: "http://127.0.0.1:43117/mcp",
      workspacePath: `/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`,
    });
  });

  test("adapter errors containing a known token are redacted and drop the cause", async () => {
    const { adapter, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    adapter.instances[0]!.closeError = new Error(`close failed with ${TOKEN}`);

    const failure = await expectErrorCode(handle.stop(), "stop_incomplete");
    expect(failure.message).not.toContain(TOKEN);
    expect(failure.message).toContain("[redacted]");
    expect(failure.cause).toBeUndefined();
  });

  test("Bearer credentials are redacted even before a token is known", async () => {
    const { adapter, manager } = createManager();
    adapter.startError = new Error("connect failed: Bearer abc123");

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "start_failed");
    expect(failure.message).toBe("connect failed: Bearer [redacted]");
  });

  test("stop closes the instance but keeps the handle and workspace", async () => {
    const { adapter, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    await handle.stop();

    expect(adapter.instances[0]?.closeCalls).toBe(1);
    expect(handle.state).toBe("stopped");
    expect(handle.status().mcpUrl).toBeNull();
    expect(handle.workspacePath).toBe(`/tmp/peri-sandbox/sandboxes/${SANDBOX_ID}/workspace`);
    expect(manager.get(SANDBOX_ID)).toBe(handle);

    await handle.start();
    expect(handle.state).toBe("ready");
    expect(adapter.starts.length).toBe(2);
  });

  test("close removes the handle and rejects a later start", async () => {
    const { adapter, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    await handle.close();

    expect(handle.state).toBe("closed");
    expect(adapter.instances[0]?.closeCalls).toBe(1);
    expect(adapter.deleteCalls).toEqual([]);
    expect(manager.get(SANDBOX_ID)).toBeUndefined();
    expect(manager.size).toBe(0);
    await expectErrorCode(handle.start(), "closed");
  });

  test("close({deleteData:true}) closes the instance then deletes adapter data", async () => {
    const { adapter, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    await handle.close({ deleteData: true });

    expect(adapter.instances[0]?.closeCalls).toBe(1);
    expect(adapter.deleteCalls).toEqual([SANDBOX_ID]);
    expect(handle.state).toBe("closed");
    expect(manager.get(SANDBOX_ID)).toBeUndefined();

    await handle.close({ deleteData: true });
    expect(adapter.deleteCalls).toEqual([SANDBOX_ID]);
  });

  test("close({deleteData:true}) skips adapters without deleteData", async () => {
    const adapter: SandboxAdapter = {
      start: async (options) => new FakeInstance(options.sandboxId, "http://127.0.0.1:43117/mcp"),
    };
    const manager = new SandboxManager({ adapter });
    const handle = await manager.start({ sandboxId: SANDBOX_ID });

    await handle.close({ deleteData: true });
    expect(handle.state).toBe("closed");
  });

  test("delete failures surface delete_incomplete and keep the handle for retry", async () => {
    const { adapter, manager } = createManager();
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    adapter.deleteError = new Error("cannot delete sandbox data");

    const failure = await expectErrorCode(handle.close({ deleteData: true }), "delete_incomplete");
    expect(failure.message).toContain("cannot delete sandbox data");
    expect(handle.state).toBe("failed");
    expect(manager.get(SANDBOX_ID)).toBe(handle);

    await handle.close({ deleteData: true });
    expect(handle.state).toBe("closed");
    expect(adapter.deleteCalls).toEqual([SANDBOX_ID, SANDBOX_ID]);
    expect(manager.get(SANDBOX_ID)).toBeUndefined();
  });

  test("closeAll closes every handle concurrently and detaches them", async () => {
    const { adapter, manager } = createManager();
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    const second = await manager.start({ sandboxId: SECOND_ID });
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    adapter.instances.forEach((instance) => {
      instance.closeGate = gate;
    });

    const closing = manager.closeAll();
    await waitFor(() => adapter.instances.every((instance) => instance.closeCalls === 1));
    release();
    await closing;

    expect(first.state).toBe("closed");
    expect(second.state).toBe("closed");
    expect(manager.size).toBe(0);
  });

  test("closeAll keeps failed handles and reports an AggregateError that can be retried", async () => {
    const { adapter, manager } = createManager();
    const first = await manager.start({ sandboxId: SANDBOX_ID });
    await manager.start({ sandboxId: SECOND_ID });
    adapter.instances[0]!.closeError = new Error("stop failed");

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
    expect(first.state).toBe("failed");

    adapter.instances[0]!.closeError = undefined;
    await manager.closeAll();
    expect(manager.size).toBe(0);
  });

  test("an adapter id mismatch is rejected as conflict and closes the instance", async () => {
    const { adapter, manager } = createManager();
    adapter.instanceId = SECOND_ID;

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "conflict");
    expect(failure.message).toContain(SECOND_ID);
    expect(adapter.instances[0]?.closeCalls).toBe(1);
    expect(manager.size).toBe(0);
  });

  test("an invalid adapter instance reports start_failed without leaking the handle", async () => {
    const manager = new SandboxManager({
      adapter: {
        start: async () => ({ sandboxId: SANDBOX_ID, workspacePath: "relative", mcpServer: { type: "http", url: "http://127.0.0.1:1/mcp", headers: {} }, close: async () => undefined }),
      } as unknown as SandboxAdapter,
    });

    const failure = await expectErrorCode(manager.start({ sandboxId: SANDBOX_ID }), "workspace_unavailable");
    expect(failure.message).toContain("absolute");
    expect(manager.size).toBe(0);
  });

  test("stop failures keep the handle failed and allow a retry", async () => {
    const { adapter, manager } = createManager({ stopTimeoutMs: 1_000 });
    const handle = await manager.start({ sandboxId: SANDBOX_ID });
    adapter.instances[0]!.closeError = new Error("cannot stop");

    await expectErrorCode(handle.stop(), "stop_incomplete");
    expect(handle.state).toBe("failed");
    expect(manager.get(SANDBOX_ID)).toBe(handle);

    adapter.instances[0]!.closeError = undefined;
    await handle.stop();
    expect(handle.state).toBe("stopped");
  });

  test("the handle state machine moves through starting, ready, stopping and closed", async () => {
    const { adapter, manager } = createManager();
    let release: () => void = () => undefined;
    adapter.startGate = new Promise<void>((resolve) => {
      release = resolve;
    });

    const pending = manager.start({ sandboxId: SANDBOX_ID });
    await waitFor(() => manager.get(SANDBOX_ID)?.state === "starting");
    release();
    const handle = await pending;
    expect(handle.state).toBe("ready");

    adapter.instances[0]!.closeDelayMs = 20;
    const closing = handle.close();
    await waitFor(() => handle.state === "stopping");
    expect(handle.endpoint()).not.toBeNull();
    await closing;
    expect(handle.state).toBe("closed");
  });
});
