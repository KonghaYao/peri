import type { Database } from 'bun:sqlite';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { Hono } from 'hono';
import { createApp } from '../../../../server/app';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import { testEnv } from '../../helpers';

export const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;
export const JSON_HEADERS = {
  ...AUTHORIZATION,
  'content-type': 'application/json',
} as const;

/** Fixed deployment key so file sealing never depends on the developer data directory. */
const TEST_CREDENTIAL_KEY = '11'.repeat(32);

export interface EnvironmentHarness {
  readonly app: Hono;
  readonly db: Database;
  readonly workspace: string;
  cleanup(): void;
}

export function setupHarness(): EnvironmentHarness {
  const workspace = realpathSync(mkdtempSync(path.join(tmpdir(), 'agent-builder-env-')));
  const previousWorkspace = process.env.PERI_WORKSPACE_PATH;
  const previousKey = process.env.PLATFORM_CREDENTIAL_KEY;
  process.env.PERI_WORKSPACE_PATH = workspace;
  process.env.PLATFORM_CREDENTIAL_KEY = TEST_CREDENTIAL_KEY;

  const db = openDatabase(':memory:');
  migrate(db);
  const app = createApp({ env: testEnv({ workspacePath: workspace }), db });

  return {
    app,
    db,
    workspace,
    cleanup() {
      db.close();
      if (previousWorkspace === undefined) delete process.env.PERI_WORKSPACE_PATH;
      else process.env.PERI_WORKSPACE_PATH = previousWorkspace;
      if (previousKey === undefined) delete process.env.PLATFORM_CREDENTIAL_KEY;
      else process.env.PLATFORM_CREDENTIAL_KEY = previousKey;
      rmSync(workspace, { recursive: true, force: true });
    },
  };
}

export async function request(
  app: Hono,
  method: 'GET' | 'POST' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<Response> {
  return app.request(`/v1${path}`, {
    method,
    headers: body === undefined ? AUTHORIZATION : JSON_HEADERS,
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

export interface StoredEnvironmentFileRow {
  readonly id: string;
  readonly environment_id: string;
  readonly path: string;
  readonly content_ciphertext: Uint8Array;
  readonly size_bytes: number;
}

export function storedFileRow(
  db: Database,
  environmentId: string,
  filePath: string,
): StoredEnvironmentFileRow | null {
  const row = db
    .query<StoredEnvironmentFileRow, [string, string]>(
      `SELECT id, environment_id, path, content_ciphertext, size_bytes
       FROM platform_environment_files WHERE environment_id = ? AND path = ?`,
    )
    .get(environmentId, filePath);
  return row ?? null;
}

export async function createEnvironment(
  app: Hono,
  workspace: string,
  body: Record<string, unknown> = {},
): Promise<Record<string, unknown>> {
  const response = await request(app, 'POST', '/agents/environments', {
    environment: { type: 'local', workspace_directory: workspace, ...body },
  });
  if (response.status !== 201) {
    throw new Error(`Environment setup failed with status ${response.status}`);
  }
  return (await response.json()) as Record<string, unknown>;
}

export function base64(text: string): string {
  return Buffer.from(text, 'utf8').toString('base64');
}
