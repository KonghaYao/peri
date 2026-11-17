import { describe, expect, test } from 'bun:test';
import * as clientEntry from '../src/client/index.js';
import * as contracts from '../src/contracts/index.js';
import {
  type ActivityCounters,
  type ActivitySnapshot,
  canSleep,
  decodePeriError,
  type FactEnvelope,
  type FactIdentity,
  type FactSnapshotFrame,
  type FactStreamFrame,
  isQuiescentCounters,
  PERI_ERROR_CODES,
  type PeriClient,
  PeriError,
  type PeriErrorCode,
  parseAttemptId,
  parseChatId,
  parseConfigRevision,
  parseSessionEpoch,
  parseSessionId,
  parseTurnId,
  type SandboxHandle,
  type SessionHandle,
} from '../src/contracts/index.js';
import * as serverIdentity from '../src/contracts/server-identity.js';

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Assert<T extends true> = T;
type ForbiddenClientKey =
  | 'actorKey'
  | 'actor_key'
  | 'activationId'
  | 'activation_id'
  | 'leaseEpoch'
  | 'lease_epoch'
  | 'runtimeId'
  | 'runtime_id';
type NoForbiddenKeys<T> = Extract<keyof T, ForbiddenClientKey> extends never ? true : false;

// 类型测试以导出的常量形式存在：tsc 在 noUnusedLocals 下也会实际检查断言结果。
export const sessionHandleShape: Assert<NoForbiddenKeys<SessionHandle>> = true;
export const sandboxHandleShape: Assert<Equal<keyof SandboxHandle, 'observedAt' | 'ready'>> = true;
export const clientGroups: Assert<Equal<keyof PeriClient, 'sessions' | 'sandboxes' | 'resources'>> = true;
export const attemptIdIsNull: Assert<Equal<FactIdentity['attemptId'], null>> = true;
export const snapshotActivity: Assert<Equal<FactSnapshotFrame['activity'], ActivitySnapshot | null>> = true;

function codeOf(run: () => unknown): PeriErrorCode {
  try {
    run();
  } catch (error) {
    expect(error).toBeInstanceOf(PeriError);
    return (error as PeriError).code;
  }
  throw new Error('expected a PeriError');
}

function activityCounters(overrides: Partial<ActivityCounters> = {}): ActivityCounters {
  return {
    rootRunning: false,
    activeInternalTasks: 0,
    queuedInputs: 0,
    pendingDeliveries: 0,
    pendingInteractions: 0,
    localOperations: 0,
    ...overrides,
  };
}

function activitySnapshot(
  overrides: Partial<ActivitySnapshot> = {},
  counterOverrides: Partial<ActivityCounters> = {},
): ActivitySnapshot {
  return {
    state: 'idle',
    complete: true,
    closing: false,
    counters: activityCounters(counterOverrides),
    pins: [],
    hold: null,
    observedAt: '2026-11-13T00:00:00.000Z',
    activeSince: null,
    idleSince: '2026-11-13T00:00:00.000Z',
    ...overrides,
  };
}

describe('contract identity', () => {
  test('accepts opaque ids, keeps epochs positive, and rejects unaddressable values', () => {
    expect(parseSessionId('01J8Z7SESSION')).toBe('01J8Z7SESSION' as ReturnType<typeof parseSessionId>);
    expect(parseTurnId('turn_1')).toBe('turn_1' as ReturnType<typeof parseTurnId>);
    expect(parseConfigRevision('2026-11-13T00:00:00.000Z')).toBe(
      '2026-11-13T00:00:00.000Z' as ReturnType<typeof parseConfigRevision>,
    );

    expect(codeOf(() => parseSessionId(''))).toBe('invalid_identity');
    expect(codeOf(() => parseSessionId('session with space'))).toBe('invalid_identity');
    expect(codeOf(() => parseSessionId(`session${String.fromCharCode(0)}id`))).toBe('invalid_identity');
    expect(codeOf(() => parseSessionId('x'.repeat(201)))).toBe('invalid_identity');

    expect(parseSessionEpoch(1)).toBe(1 as ReturnType<typeof parseSessionEpoch>);
    expect(codeOf(() => parseSessionEpoch(0))).toBe('invalid_identity');
    expect(serverIdentity.parseSandboxId('sb_1')).toBe(
      'sb_1' as ReturnType<typeof serverIdentity.parseSandboxId>,
    );
    expect(codeOf(() => serverIdentity.parseSandboxId(''))).toBe('invalid_identity');

    expect(parseAttemptId(null)).toBeNull();
    expect(parseAttemptId('attempt_1')).toBe('attempt_1' as ReturnType<typeof parseAttemptId>);
    expect(codeOf(() => parseAttemptId(''))).toBe('invalid_identity');
  });

  test('chat ids follow the same identity rules as sessions but stay a distinct brand', () => {
    const chatId = parseChatId('rt_9f1c');
    expect(chatId).toBe('rt_9f1c' as ReturnType<typeof parseChatId>);
    // The brand keeps a chat key from being passed where an ACP Session id is expected.
    type ChatIsNotSession =
      Equal<typeof chatId, ReturnType<typeof parseSessionId>> extends true ? false : true;
    const distinct = true satisfies ChatIsNotSession;
    expect(distinct).toBe(true);
    expect(codeOf(() => parseChatId(''))).toBe('invalid_identity');
    expect(codeOf(() => parseChatId(' chat_1'))).toBe('invalid_identity');
    expect(codeOf(() => parseChatId('chat_1\n'))).toBe('invalid_identity');
    expect(codeOf(() => parseChatId('x'.repeat(201)))).toBe('invalid_identity');
  });
});

describe('PeriError', () => {
  test('survives a JSON round-trip without serializing upstream messages', () => {
    const upstream = new Error('secret-token-123');
    upstream.cause = new TypeError('nested-secret');
    const original = new PeriError({
      code: 'sandbox_lease_stale',
      phase: 'sandbox',
      message: 'sandbox lease is stale',
      details: { lease_id: 'lease_1', epoch: 3 },
      session_id: 'session_1',
      session_epoch: 1,
      turn_id: 'turn_1',
      attempt_id: null,
      cause: upstream,
    });

    const wire = JSON.parse(JSON.stringify(original));
    const decoded = decodePeriError(wire);

    expect(decoded).toBeInstanceOf(PeriError);
    expect(decoded.toJSON()).toEqual(original.toJSON());
    expect(decoded.retryable).toBe(false);
    expect(wire.cause).toEqual([{ name: 'Error' }, { name: 'TypeError' }]);
    expect(JSON.stringify(wire)).not.toContain('secret-token-123');
    expect(JSON.stringify(wire)).not.toContain('nested-secret');
  });

  test('maps retryable from the code unless explicitly overridden', () => {
    expect(new PeriError({ code: 'busy', phase: 'harness', message: 'busy' }).retryable).toBe(true);
    expect(new PeriError({ code: 'binding_conflict', phase: 'session', message: 'conflict' }).retryable).toBe(
      false,
    );
    expect(
      new PeriError({ code: 'wake_failed', phase: 'harness', message: 'wake failed', retryable: false })
        .retryable,
    ).toBe(false);
  });

  test('decodes directory codes and keeps storage failures retryable', () => {
    const cases = [
      { code: 'binding_conflict', retryable: false },
      { code: 'invalid_identity', retryable: false },
      { code: 'storage_unavailable', retryable: true },
    ] as const;

    for (const entry of cases) {
      const decoded = decodePeriError({
        name: 'PeriError',
        code: entry.code,
        phase: 'session',
        message: entry.code,
        retryable: entry.retryable,
      });
      expect(decoded.code).toBe(entry.code);
      expect(decoded.toJSON().code).toBe(entry.code);
      expect(decoded.retryable).toBe(entry.retryable);
    }

    expect(decodePeriError({ code: 'storage_unavailable', message: 'down' }).retryable).toBe(true);
    expect(PERI_ERROR_CODES).not.toContain('session_busy' as never);
    expect(PERI_ERROR_CODES).not.toContain('session_epoch_mismatch' as never);
  });

  test('degrades unknown codes to internal_error while keeping the original code', () => {
    const decoded = decodePeriError({
      code: 'session_epoch_mismatch',
      phase: 'session',
      message: 'epoch mismatch',
      retryable: true,
    });

    expect(decoded.code).toBe('internal_error');
    expect(decoded.details).toEqual({ original_code: 'session_epoch_mismatch' });
    expect(decoded.retryable).toBe(false);
    expect(decodePeriError('not-an-error').code).toBe('internal_error');
    expect(decodePeriError(null).code).toBe('internal_error');
  });
});

describe('fact frames', () => {
  const identity: FactIdentity = {
    sessionId: parseSessionId('session_1'),
    sessionEpoch: parseSessionEpoch(1),
    turnId: parseTurnId('turn_1'),
    attemptId: null,
  };
  const emittedAt = '2026-11-13T00:00:00.000Z';

  test('distinguishes snapshot, reset and overflow frames and carries activity', () => {
    const envelope: FactEnvelope = {
      identity,
      fact: { kind: 'run-state', running: false, executionBlocked: false },
      emittedAt,
    };
    expect(envelope.identity.attemptId).toBeNull();
    expect('type' in envelope).toBe(false);
    const snapshotFrame: FactStreamFrame = {
      type: 'snapshot',
      identity,
      facts: [envelope],
      activity: activitySnapshot(),
      emittedAt,
    };
    const resetFrame: FactStreamFrame = {
      type: 'reset',
      identity,
      reason: 'takeover',
      emittedAt,
    };
    const overflowFrame: FactStreamFrame = {
      type: 'overflow',
      identity,
      reason: 'overflow',
      emittedAt,
    };

    expect(snapshotFrame.type).toBe('snapshot');
    expect(snapshotFrame.activity?.state).toBe('idle');
    expect(resetFrame.type).toBe('reset');
    expect(overflowFrame.type).toBe('overflow');
  });
});

describe('canSleep', () => {
  test('allows sleep only for a complete, idle snapshot and rejects every blocking condition', () => {
    expect(canSleep(activitySnapshot())).toBe(true);
    expect(isQuiescentCounters(activityCounters())).toBe(true);
    const blocked = [
      activitySnapshot({ state: 'active' }),
      activitySnapshot({ complete: false }),
      activitySnapshot({ closing: true }),
      activitySnapshot({ hold: { since: '2026-11-13T00:00:00.000Z' } }),
      activitySnapshot({ pins: ['activity_source_missing'] }),
      activitySnapshot({ pins: ['observation_incomplete'] }),
      activitySnapshot({}, { rootRunning: true }),
      activitySnapshot({}, { activeInternalTasks: 1 }),
      activitySnapshot({}, { queuedInputs: 1 }),
      activitySnapshot({}, { pendingDeliveries: 1 }),
      activitySnapshot({}, { pendingInteractions: 1 }),
      activitySnapshot({}, { localOperations: 1 }),
    ];
    for (const candidate of blocked) {
      expect(canSleep(candidate)).toBe(false);
    }
  });
});

describe('client contract', () => {
  test('handles expose only client-visible identity and public entries hide server identities', () => {
    const handle: SessionHandle = {
      sessionId: parseSessionId('session_1'),
      sessionEpoch: null,
      configRevision: parseConfigRevision('2026-11-13T00:00:00.000Z'),
    };
    const sandbox: SandboxHandle = { ready: true, observedAt: '2026-11-13T00:00:00.000Z' };

    expect(Object.keys(handle).sort()).toEqual(['configRevision', 'sessionEpoch', 'sessionId']);
    expect(Object.keys(sandbox).sort()).toEqual(['observedAt', 'ready']);

    // @ts-expect-error leaseEpoch 不属于客户端契约
    const withLease: SessionHandle = { ...handle, leaseEpoch: 1 };
    expect(Object.keys(withLease).sort()).toEqual([
      'configRevision',
      'leaseEpoch',
      'sessionEpoch',
      'sessionId',
    ]);

    expect('PeriClient' in contracts).toBe(false);
    expect('createPeriClient' in contracts).toBe(false);
    for (const parser of ['parseLeaseEpoch', 'parseHarnessRef', 'parseActivationId', 'parseSandboxId']) {
      expect(parser in contracts).toBe(false);
    }
    expect('PERI_RETRYABILITY' in contracts).toBe(false);
    expect(typeof contracts.PeriError).toBe('function');
    expect(Object.keys(clientEntry)).toEqual([]);
  });
});
