import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDatabase } from '../../../server/db/database';
import { migrate, SESSION_INLINE_AGENT_ID } from '../../../server/db/migrate';
import {
  CredentialDecryptionError,
  CredentialKeyMismatchError,
  decryptCredential,
  encryptCredential,
} from '../../../server/platform/hashing';
import { isUuidV7 } from '../../../server/platform/ids';
import type { PlatformRepositories } from '../../../server/platform/repositories';
import { createPlatformRepositories } from '../../../server/platform/repositories';
import type { ListQuery } from '../../../server/platform/repositories/shared';

const PAGE: ListQuery = { limit: 20, order: 'asc', after: null };
const KEY = Buffer.alloc(32, 9);
const ROTATED_KEY = Buffer.alloc(32, 10);

let db: Database | null = null;
const tempDirs: string[] = [];

afterEach(() => {
  db?.close();
  db = null;
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function setup(): PlatformRepositories {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  return createPlatformRepositories(database);
}

function tempDir(): string {
  const dir = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-platform-restart-')));
  tempDirs.push(dir);
  return dir;
}

function createSession(repos: PlatformRepositories, id = 'sess-1') {
  return repos.sessions.create({
    id,
    agentId: SESSION_INLINE_AGENT_ID,
    agentConfig: '{"instructions":"inline"}',
    environmentType: 'none',
  });
}

interface SeededIds {
  readonly sessionId: string;
  readonly turnId: string;
  readonly itemId: string;
  readonly eventSeq: number;
}

/** Writes one session graph and closes the database, mirroring a process restart. */
function seedFileDatabase(databasePath: string): SeededIds {
  const database = openDatabase(databasePath);
  try {
    migrate(database);
    const repos = createPlatformRepositories(database);
    const session = createSession(repos, 'sess-restart');
    const turn = repos.turns.create({
      sessionId: session.id,
      agentId: SESSION_INLINE_AGENT_ID,
      runtimeTurnId: 'runtime-restart',
      bindingRevision: 2,
    });
    const item = repos.items.create({
      sessionId: session.id,
      turnId: turn.id,
      outputIndex: 3,
      type: 'tool_call',
      status: 'completed',
      callId: 'call-1',
      payload: '{"name":"read"}',
    });
    const event = repos.events.append({
      sessionId: session.id,
      type: 'item.completed',
      turnId: turn.id,
      itemId: item.id,
      data: '{"ok":true}',
    });
    return { sessionId: session.id, turnId: turn.id, itemId: item.id, eventSeq: event.seq };
  } finally {
    database.close();
  }
}

describe('platform repositories', () => {
  test('mints UUIDv7 ids for platform agents and round-trips the extended columns', () => {
    const repos = setup();
    const created = repos.agents.create({
      name: 'Reviewer',
      description: 'Checks changes',
      config: '{"instructions":"Be concise"}',
      model: 'model-1',
      metadata: '{"team":"quality"}',
    });

    expect(isUuidV7(created.id)).toBe(true);
    expect(repos.agents.get(created.id)?.model).toBe('model-1');
    // The inline session placeholder is an ordinary row, so it appears alongside created agents.
    expect(repos.agents.list(PAGE).map((row) => row.id)).toEqual([SESSION_INLINE_AGENT_ID, created.id]);
    expect(repos.agents.delete(created.id)).toBe(true);
    expect(repos.agents.get(created.id)).toBeNull();
  });

  test('round-trips sessions, turns, items and events with millisecond timestamps', () => {
    const repos = setup();
    const session = createSession(repos);
    const turn = repos.turns.create({
      sessionId: session.id,
      agentId: SESSION_INLINE_AGENT_ID,
      runtimeTurnId: 'runtime-turn-1',
      bindingRevision: 1,
      startedAt: 1_700_000_000_100,
    });
    const item = repos.items.create({
      sessionId: session.id,
      turnId: turn.id,
      outputIndex: 0,
      type: 'message',
      status: 'completed',
      payload: '{"text":"hi"}',
    });
    const event = repos.events.append({
      sessionId: session.id,
      type: 'item.completed',
      turnId: turn.id,
      itemId: item.id,
      data: '{}',
    });

    expect(session.agent_id).toBe(SESSION_INLINE_AGENT_ID);
    expect(session.environment_type).toBe('none');
    expect(session.status).toBe('idle');
    expect(session.created_at).toBeGreaterThan(1_600_000_000_000);
    expect(session.created_at).toBe(session.last_active_at);
    expect(turn.runtime_turn_id).toBe('runtime-turn-1');
    expect(turn.agent_id).toBe(SESSION_INLINE_AGENT_ID);
    expect(item.output_index).toBe(0);
    expect(event.turn_id).toBe(turn.id);
    expect(event.item_id).toBe(item.id);
    expect(isUuidV7(event.event_id)).toBe(true);

    expect(repos.turns.listBySession(session.id).map((row) => row.id)).toEqual([turn.id]);
    expect(repos.items.listBySession(session.id).map((row) => row.id)).toEqual([item.id]);
    expect(repos.items.listByTurn(turn.id).map((row) => row.id)).toEqual([item.id]);
    expect(repos.events.listBySession(session.id).map((row) => row.seq)).toEqual([event.seq]);

    expect(repos.turns.complete(turn.id, 'completed', { stopReason: 'end_turn' })).toMatchObject({
      status: 'completed',
      stop_reason: 'end_turn',
    });
    expect(repos.sessions.setStatus(session.id, 'failed', 'boom')?.status).toBe('failed');

    expect(repos.sessions.delete(session.id)).toBe(true);
    expect(repos.turns.listBySession(session.id)).toEqual([]);
    expect(repos.items.listBySession(session.id)).toEqual([]);
    expect(repos.events.listBySession(session.id)).toEqual([]);
  });

  test('round-trips subagents and scopes turns and items to them', () => {
    const repos = setup();
    const session = createSession(repos);
    const subagent = repos.subagents.create({
      sessionId: session.id,
      name: 'researcher',
      instructions: 'Look things up',
      parentAgentId: SESSION_INLINE_AGENT_ID,
    });
    const subagentTurn = repos.turns.create({
      sessionId: session.id,
      agentId: SESSION_INLINE_AGENT_ID,
      subagentId: subagent.id,
      runtimeTurnId: 'runtime-subagent',
      bindingRevision: 1,
    });
    const parentTurn = repos.turns.create({
      sessionId: session.id,
      agentId: SESSION_INLINE_AGENT_ID,
      runtimeTurnId: 'runtime-parent',
      bindingRevision: 1,
    });
    const subagentItem = repos.items.create({
      sessionId: session.id,
      turnId: subagentTurn.id,
      outputIndex: 0,
      type: 'message',
      status: 'completed',
      payload: '{"text":"found"}',
    });
    repos.items.create({
      sessionId: session.id,
      turnId: parentTurn.id,
      outputIndex: 0,
      type: 'message',
      status: 'completed',
      payload: '{"text":"parent"}',
    });

    expect(isUuidV7(subagent.id)).toBe(true);
    expect(repos.subagents.get(subagent.id)?.status).toBe('active');
    expect(repos.subagents.listBySession(session.id).map((row) => row.id)).toEqual([subagent.id]);
    expect(repos.turns.listBySubagent(session.id, subagent.id).map((row) => row.id)).toEqual([
      subagentTurn.id,
    ]);
    expect(repos.items.listBySubagent(session.id, subagent.id).map((row) => row.id)).toEqual([
      subagentItem.id,
    ]);
    expect(repos.subagents.close(subagent.id)?.status).toBe('closed');
  });

  test('replays events by sequence and resumes after the last seen seq', () => {
    const repos = setup();
    const session = createSession(repos);
    const first = repos.events.append({ sessionId: session.id, type: 'session.created', data: '{}' });
    const second = repos.events.append({ sessionId: session.id, type: 'turn.started', data: '{}' });
    const third = repos.events.append({ sessionId: session.id, type: 'turn.completed', data: '{}' });

    expect(repos.events.listBySession(session.id).map((row) => row.seq)).toEqual([
      first.seq,
      second.seq,
      third.seq,
    ]);
    expect(repos.events.listBySession(session.id, first.seq).map((row) => row.seq)).toEqual([
      second.seq,
      third.seq,
    ]);
    // Replaying the same cursor twice is idempotent: no new rows, same order.
    expect(repos.events.listBySession(session.id, first.seq)).toEqual(
      repos.events.listBySession(session.id, first.seq),
    );
  });

  test('keeps sessions, turns, items and events queryable after a restart', () => {
    const dir = tempDir();
    const databasePath = path.join(dir, 'agent-builder.db');
    const seeded = seedFileDatabase(databasePath);

    const reopened = openDatabase(databasePath);
    try {
      migrate(reopened);
      const repos = createPlatformRepositories(reopened);
      expect(repos.sessions.get(seeded.sessionId)?.agent_config).toBe('{"instructions":"inline"}');
      expect(repos.turns.get(seeded.turnId)?.binding_revision).toBe(2);
      expect(repos.items.get(seeded.itemId)?.call_id).toBe('call-1');
      expect(repos.items.listByTurn(seeded.turnId).map((row) => row.output_index)).toEqual([3]);
      expect(repos.events.listBySession(seeded.sessionId).map((row) => row.seq)).toEqual([seeded.eventSeq]);
    } finally {
      reopened.close();
    }
  });

  test('round-trips vaults and encrypted credential quartets', () => {
    const repos = setup();
    const vault = repos.vaults.create({ name: 'Default' });
    const secret = encryptCredential('token-value', KEY);
    const credential = repos.credentials.create({
      vaultId: vault.id,
      name: 'token',
      authKind: 'static_bearer',
      auth: 'Authorization',
      secret,
    });

    expect(repos.vaults.list(PAGE).map((row) => row.id)).toEqual([vault.id]);
    expect(repos.credentials.listByVault(vault.id).map((row) => row.id)).toEqual([credential.id]);
    expect(credential.key_id).toBe(secret.keyId);
    expect(Buffer.from(credential.secret_cipher)).toEqual(Buffer.from(secret.cipher));
    expect(Buffer.from(credential.secret_iv)).toEqual(Buffer.from(secret.iv));
    expect(Buffer.from(credential.secret_tag)).toEqual(Buffer.from(secret.tag));

    const stored = {
      cipher: credential.secret_cipher,
      iv: credential.secret_iv,
      tag: credential.secret_tag,
      keyId: credential.key_id,
    };
    expect(decryptCredential(stored, KEY)).toBe('token-value');
    expect(() => decryptCredential(stored, ROTATED_KEY)).toThrow(CredentialKeyMismatchError);

    const rotated = encryptCredential('rotated-value', ROTATED_KEY);
    expect(
      repos.credentials.update(credential.id, {
        name: null,
        authKind: null,
        auth: null,
        secret: rotated,
        metadata: null,
      })?.key_id,
    ).toBe(rotated.keyId);

    expect(repos.vaults.delete(vault.id)).toBe(true);
    expect(repos.credentials.get(credential.id)).toBeNull();
  });

  test('rejects tampered credential payloads read back from storage', () => {
    const repos = setup();
    const vault = repos.vaults.create({ name: 'Default' });
    const credential = repos.credentials.create({
      vaultId: vault.id,
      name: 'token',
      authKind: 'environment_variable',
      auth: 'API_TOKEN',
      secret: encryptCredential('token-value', KEY),
    });
    const tamperedCipher = Uint8Array.from(credential.secret_cipher);
    tamperedCipher[0] = (tamperedCipher[0] ?? 0) ^ 0xff;

    expect(() =>
      decryptCredential(
        {
          cipher: tamperedCipher,
          iv: credential.secret_iv,
          tag: credential.secret_tag,
          keyId: credential.key_id,
        },
        KEY,
      ),
    ).toThrow(CredentialDecryptionError);
  });

  test('round-trips environments, templates and sealed files', () => {
    const repos = setup();
    const environment = repos.environments.create({
      name: 'Default',
      type: 'self_hosted',
      config: '{}',
    });
    const template = repos.environmentTemplates.create({ name: 'Base', config: '{}' });
    const sealed = new Uint8Array([1, 2, 3, 4]);
    const file = repos.environmentFiles.create({
      environmentId: environment.id,
      path: '/workspace/notes.txt',
      contentCiphertext: sealed,
      sizeBytes: 4,
    });

    expect(environment.status).toBe('pending');
    expect(repos.environments.list(PAGE).map((row) => row.id)).toEqual([environment.id]);
    expect(repos.environmentTemplates.list(PAGE).map((row) => row.id)).toEqual([template.id]);
    expect(Buffer.from(file.content_ciphertext)).toEqual(Buffer.from(sealed));
    expect(repos.environmentFiles.listByEnvironment(environment.id).map((row) => row.id)).toEqual([file.id]);

    expect(
      repos.environments.update(environment.id, {
        name: 'Renamed',
        type: null,
        status: 'ready',
        config: null,
      }),
    ).toMatchObject({ name: 'Renamed', status: 'ready' });
    expect(repos.environmentFiles.delete(file.id)).toBe(true);
    expect(repos.environmentFiles.listByEnvironment(environment.id)).toEqual([]);
    expect(repos.environments.delete(environment.id)).toBe(true);
    expect(repos.environmentTemplates.delete(template.id)).toBe(true);
  });

  test('round-trips automations, pause state and deduplicated invocations', () => {
    const repos = setup();
    const automation = repos.automations.create({
      type: 'periodic',
      name: 'Nightly',
      target: '{"kind":"session"}',
      expression: '0 0 * * *',
      timezone: 'UTC',
      nextRunAt: 1_700_000_000_000,
      createdBy: SESSION_INLINE_AGENT_ID,
    });
    const invocation = repos.automationInvocations.create({
      automationId: automation.id,
      source: 'schedule',
      scheduledAt: 1_700_000_000_000,
    });

    expect(repos.automations.list(PAGE).map((row) => row.id)).toEqual([automation.id]);
    expect(repos.automationInvocations.getBySchedule(automation.id, 1_700_000_000_000)?.id).toBe(
      invocation.id,
    );
    expect(() =>
      repos.automationInvocations.create({
        automationId: automation.id,
        source: 'schedule',
        scheduledAt: 1_700_000_000_000,
      }),
    ).toThrow();

    expect(repos.automations.pause(automation.id, 'manual')?.status).toBe('paused');
    expect(repos.automations.resume(automation.id)).toMatchObject({
      status: 'active',
      pause_reason: null,
    });
    expect(
      repos.automationInvocations.complete(invocation.id, {
        status: 'failed',
        sessionId: null,
        errorCode: 'engine_error',
        errorMessage: 'boom',
      }),
    ).toMatchObject({ status: 'failed', error_code: 'engine_error' });

    expect(repos.automations.delete(automation.id)).toBe(true);
    expect(repos.automationInvocations.get(invocation.id)).toBeNull();
  });

  test('round-trips artifacts and traces', () => {
    const repos = setup();
    const session = createSession(repos);
    const artifact = repos.artifacts.create({
      sessionId: session.id,
      environmentId: 'env-1',
      turnId: 'turn-1',
      path: 'report.txt',
      mediaType: 'text/plain',
      sizeBytes: 12,
    });
    const trace = repos.traces.create({
      sessionId: session.id,
      turnId: null,
      kind: 'model_call',
      data: '{}',
    });

    expect(repos.artifacts.listBySession(session.id).map((row) => row.id)).toEqual([artifact.id]);
    expect(repos.artifacts.get(artifact.id)).toMatchObject({
      environment_id: 'env-1',
      turn_id: 'turn-1',
    });
    expect(repos.traces.listBySession(session.id).map((row) => row.id)).toEqual([trace.id]);
    expect(repos.artifacts.delete(artifact.id)).toBe(true);
    expect(repos.artifacts.get(artifact.id)).toBeNull();
  });
});
