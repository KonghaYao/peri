import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { TestContext } from 'node:test';
import type { SandboxInstance, SandboxState, SandboxStatus, StartSandboxOptions } from '../../src/instance/types.js';

export interface FakeStartCall {
  readonly options: StartSandboxOptions;
  readonly runId: string;
  readonly token: string;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly mcpUrl: string;
}

export interface FakeStarter {
  readonly calls: FakeStartCall[];
  readonly instances: FakeSandboxInstance[];
  readonly start: (options: StartSandboxOptions) => Promise<SandboxInstance>;
}

export interface FakeStarterOptions {
  /** Simulates a slow backend so concurrent callers overlap. */
  readonly delayMs?: number;
  readonly mcpUrl?: (input: { index: number; sandboxId: string }) => string;
}

export class FakeSandboxInstance implements SandboxInstance {
  readonly id: string;
  readonly runId: string;
  readonly token: string;
  readonly tokenFile = null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly mcpUrl: string;
  readonly runtime = { provider: 'macos-process-group' as const, unit: null, pid: null };
  readonly closedWith: Array<{ deleteData: boolean }> = [];
  private stateValue: SandboxState = 'ready';

  constructor(input: {
    sandboxId: string;
    runId: string;
    token: string;
    workspacePath: string;
    sandboxRoot: string;
    mcpUrl: string;
  }) {
    this.id = input.sandboxId;
    this.runId = input.runId;
    this.token = input.token;
    this.workspacePath = input.workspacePath;
    this.sandboxRoot = input.sandboxRoot;
    this.mcpUrl = input.mcpUrl;
  }

  get state(): SandboxState {
    return this.stateValue;
  }

  status(): SandboxStatus {
    return {
      id: this.id,
      runId: this.runId,
      state: this.stateValue,
      mcpUrl: this.mcpUrl,
      workspacePath: this.workspacePath,
      sandboxRoot: this.sandboxRoot,
      runtime: { ...this.runtime },
      lastError: null,
    };
  }

  async close(options: { deleteData?: boolean } = {}): Promise<void> {
    const deleteData = options.deleteData === true;
    this.closedWith.push({ deleteData });
    if (this.stateValue === 'closed') {
      if (deleteData) rmSync(this.sandboxRoot, { recursive: true, force: true });
      return;
    }
    this.stateValue = 'closed';
    if (deleteData) rmSync(this.sandboxRoot, { recursive: true, force: true });
  }
}

/** Minimal `startSandbox` replacement: creates the managed layout and returns a fake instance. */
export function createFakeStarter(options: FakeStarterOptions = {}): FakeStarter {
  const calls: FakeStartCall[] = [];
  const instances: FakeSandboxInstance[] = [];
  const start = async (startOptions: StartSandboxOptions): Promise<SandboxInstance> => {
    const sandboxId = startOptions.sandboxId;
    const sandboxesRoot = startOptions.sandboxesRoot;
    if (sandboxId === undefined || sandboxesRoot === undefined)
      throw new Error('fake startSandbox requires sandboxId and sandboxesRoot');
    if (options.delayMs !== undefined) await new Promise((resolve) => setTimeout(resolve, options.delayMs));
    const sandboxRoot = join(sandboxesRoot, sandboxId);
    const workspacePath = startOptions.workspacePath ?? join(sandboxRoot, 'workspace');
    mkdirSync(workspacePath, { recursive: true, mode: 0o700 });
    const index = calls.length;
    const runId = `run_fake_${index + 1}`;
    const token = `inner-token-${index + 1}`;
    const mcpUrl = options.mcpUrl?.({ index, sandboxId }) ?? 'http://127.0.0.1:1/mcp';
    calls.push({ options: startOptions, runId, token, workspacePath, sandboxRoot, mcpUrl });
    const instance = new FakeSandboxInstance({ sandboxId, runId, token, workspacePath, sandboxRoot, mcpUrl });
    instances.push(instance);
    return instance;
  };
  return { calls, instances, start };
}

/** Fresh root under the OS temp dir; the manager creates the leaf as 0700. */
export function temporarySwarmRoot(t: TestContext): string {
  const base = mkdtempSync(join(tmpdir(), 'peri-swarm-test-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  return join(base, 'sandboxes');
}
