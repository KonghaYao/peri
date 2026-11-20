import type { Database } from 'bun:sqlite';
import { createApp } from '../../../../server/app';
import { openDatabase } from '../../../../server/db/database';
import { migrate, SESSION_INLINE_AGENT_ID } from '../../../../server/db/migrate';
import {
  type InstalledAutomationRuntime,
  installAutomationRuntime,
} from '../../../../server/platform/automations/runtime';
import type {
  AutomationSessionCreateInput,
  AutomationSessionPort,
} from '../../../../server/platform/automations/session-port';
import {
  createPlatformRepositories,
  type PlatformRepositories,
} from '../../../../server/platform/repositories';
import type {
  PlatformSessionRow,
  PlatformSessionStatus,
} from '../../../../server/platform/repositories/sessions';
import { testEnv } from '../../helpers';

export const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

export const START = Date.UTC(2026, 0, 1, 12, 0, 30);

export interface Deferred {
  readonly promise: Promise<void>;
  readonly release: () => void;
}

export function deferred(): Deferred {
  let release: () => void = () => {};
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

export class FakeSessionPort implements AutomationSessionPort {
  readonly created: AutomationSessionCreateInput[] = [];
  readonly sends: { readonly sessionId: string; readonly text: string }[] = [];
  sendGate: Deferred | null = null;
  sendError: Error | null = null;

  async create(input: AutomationSessionCreateInput): Promise<{ sessionId: string }> {
    this.created.push(input);
    return { sessionId: `session-${this.created.length}` };
  }

  async send(sessionId: string, text: string): Promise<void> {
    this.sends.push({ sessionId, text });
    if (this.sendGate !== null) await this.sendGate.promise;
    if (this.sendError !== null) throw this.sendError;
  }
}

export interface Clock {
  now: number;
}

export interface Harness {
  readonly db: Database;
  readonly repos: PlatformRepositories;
  readonly port: FakeSessionPort;
  readonly clock: Clock;
  readonly runtime: InstalledAutomationRuntime;
  readonly app: ReturnType<typeof createApp>;
  readonly dispose: () => void;
}

export interface SetupOptions {
  readonly port?: FakeSessionPort;
  readonly missedGraceMs?: number;
  readonly now?: number;
}

export function setup(options: SetupOptions = {}): Harness {
  const db = openDatabase(':memory:');
  migrate(db);
  const repos = createPlatformRepositories(db);
  const port = options.port ?? new FakeSessionPort();
  const clock: Clock = { now: options.now ?? START };
  const runtime = installAutomationRuntime({
    repos,
    port,
    now: () => clock.now,
    ...(options.missedGraceMs === undefined ? {} : { missedGraceMs: options.missedGraceMs }),
  });
  const app = createApp({ env: testEnv(), db });
  return {
    db,
    repos,
    port,
    clock,
    runtime,
    app,
    dispose: () => {
      void runtime.stop();
      runtime.restore();
      db.close();
    },
  };
}

export interface CallResult<T> {
  readonly status: number;
  readonly body: T;
  readonly headers: Headers;
}

export async function call<T>(
  harness: Harness,
  method: 'GET' | 'POST' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<CallResult<T>> {
  const response = await harness.app.request(`/v1${path}`, {
    method,
    headers: body === undefined ? AUTHORIZATION : { ...AUTHORIZATION, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return {
    status: response.status,
    body: (await response.json()) as T,
    headers: response.headers,
  };
}

export function get<T>(harness: Harness, path: string): Promise<CallResult<T>> {
  return call(harness, 'GET', path);
}

export function post<T>(harness: Harness, path: string, body?: unknown): Promise<CallResult<T>> {
  return call(harness, 'POST', path, body);
}

export function del<T>(harness: Harness, path: string): Promise<CallResult<T>> {
  return call(harness, 'DELETE', path);
}

export function createSessionRow(
  repos: PlatformRepositories,
  status: PlatformSessionStatus = 'idle',
): PlatformSessionRow {
  return repos.sessions.create({
    agentId: SESSION_INLINE_AGENT_ID,
    agentConfig: '{}',
    environmentType: 'none',
    status,
  });
}

export function automationBody(
  sessionId: string,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    type: 'periodic',
    name: 'Daily review',
    target: { type: 'existing_session', session_id: sessionId, instructions: 'Run the review' },
    expression: '0 9 * * *',
    timezone: 'UTC',
    ...overrides,
  };
}

export function nowSeconds(harness: Harness): number {
  return Math.floor(harness.clock.now / 1000);
}
