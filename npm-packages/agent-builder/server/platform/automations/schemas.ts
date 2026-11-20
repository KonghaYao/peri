import { z } from 'zod';
import { CreateSessionParams } from '../sessions/params';
import { ENVIRONMENT_TYPE_MAPPED_REASON } from './session-port';

const MAX_TIMESTAMP_SECONDS = Math.floor(Number.MAX_SAFE_INTEGER / 1000);

const AutomationMetadata = z
  .record(z.string().min(1).max(64), z.string().max(512))
  .refine((value) => Object.keys(value).length <= 16, 'metadata accepts at most 16 pairs');

/**
 * Reuses the documented session parameter schemas for a new-session target, so agent, environment
 * and input validation stays in one place. Timing and streaming fields do not belong to a target.
 */
const NewSessionTargetParam = CreateSessionParams.pick({
  environment: true,
  agent: true,
  agent_id: true,
  input: true,
  vault_ids: true,
  metadata: true,
}).extend({ type: z.literal('new_session') });

const ExistingSessionTargetParam = z.strictObject({
  type: z.literal('existing_session'),
  session_id: z.string().min(1).max(1_048_576),
  instructions: z.string().max(1_048_576),
});

export const AutomationTargetParam = z.discriminatedUnion('type', [
  NewSessionTargetParam,
  ExistingSessionTargetParam,
]);
export type AutomationTargetParam = z.infer<typeof AutomationTargetParam>;

const UnixSecondsParam = z
  .number()
  .int()
  .min(0)
  .max(MAX_TIMESTAMP_SECONDS)
  .refine(Number.isSafeInteger, 'run_at must be a safe integer');

export const CreateAutomationParams = z.discriminatedUnion('type', [
  z.strictObject({
    type: z.literal('periodic'),
    name: z.string().min(1).max(256),
    target: AutomationTargetParam,
    metadata: AutomationMetadata.optional(),
    expression: z.string().min(1).max(256),
    timezone: z.string().min(1).max(128),
  }),
  z.strictObject({
    type: z.literal('once'),
    name: z.string().min(1).max(256),
    target: AutomationTargetParam,
    metadata: AutomationMetadata.optional(),
    run_at: UnixSecondsParam,
  }),
]);
export type CreateAutomationParams = z.infer<typeof CreateAutomationParams>;

export const UpdateAutomationParams = z.strictObject({
  name: z.string().min(1).max(256).optional(),
  target: AutomationTargetParam.optional(),
  metadata: AutomationMetadata.optional(),
  expression: z.string().min(1).max(256).optional(),
  timezone: z.string().min(1).max(128).optional(),
  run_at: UnixSecondsParam.optional(),
});
export type UpdateAutomationParams = z.infer<typeof UpdateAutomationParams>;

export interface StoredNewSessionTarget {
  readonly type: 'new_session';
  readonly environment: CreateSessionParams['environment'];
  readonly input: CreateSessionParams['input'];
  readonly agent_id?: string;
  readonly agent?: CreateSessionParams['agent'];
  readonly vault_ids?: CreateSessionParams['vault_ids'];
  readonly metadata?: CreateSessionParams['metadata'];
}

export interface StoredExistingSessionTarget {
  readonly type: 'existing_session';
  readonly session_id: string;
  readonly instructions: string;
}

export type StoredAutomationTarget = StoredNewSessionTarget | StoredExistingSessionTarget;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

const LOCAL_ENVIRONMENT_TYPES: ReadonlySet<string> = new Set(['none', 'self_hosted', 'local']);

/**
 * Reuses the session rule for hosting types this deployment cannot serve: an unknown type runs in
 * the local sandbox instead of failing the request, and the response reports the mapping through
 * `x-platform-degraded`.
 */
export function normalizeTargetEnvironment(
  environment: CreateSessionParams['environment'],
): CreateSessionParams['environment'] {
  if (LOCAL_ENVIRONMENT_TYPES.has(environment.type)) return environment;
  const unsupported = environment as {
    readonly workspace_directory?: string;
    readonly capability_directories?: readonly string[] | null;
  };
  return {
    type: 'local',
    ...(unsupported.workspace_directory === undefined
      ? {}
      : { workspace_directory: unsupported.workspace_directory }),
    ...(unsupported.capability_directories === undefined || unsupported.capability_directories === null
      ? {}
      : { capability_directories: [...unsupported.capability_directories] }),
  };
}

/** Degradations of a stored target environment; empty when the documented shape was kept as-is. */
export function targetEnvironmentDegradation(
  environment: CreateSessionParams['environment'],
): readonly string[] {
  return normalizeTargetEnvironment(environment).type === 'local' ? [ENVIRONMENT_TYPE_MAPPED_REASON] : [];
}

/** Persists a validated target; absent optional fields stay absent instead of becoming null. */
export function serializeTarget(target: AutomationTargetParam): string {
  if (target.type === 'existing_session') {
    return JSON.stringify({
      type: target.type,
      session_id: target.session_id,
      instructions: target.instructions,
    });
  }
  return JSON.stringify({
    type: target.type,
    environment: normalizeTargetEnvironment(target.environment),
    input: target.input,
    ...(target.agent_id === undefined ? {} : { agent_id: target.agent_id }),
    ...(target.agent === undefined ? {} : { agent: target.agent }),
    ...(target.vault_ids === undefined ? {} : { vault_ids: target.vault_ids }),
    ...(target.metadata === undefined ? {} : { metadata: target.metadata }),
  });
}

/** Reads a target written by `serializeTarget`; a corrupt column is a server fault, not input. */
export function parseStoredTarget(raw: string): StoredAutomationTarget {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error('Stored automation target is not valid JSON');
  }
  if (!isRecord(parsed)) throw new Error('Stored automation target is not an object');
  if (parsed.type === 'existing_session') {
    const sessionId = parsed.session_id;
    if (typeof sessionId !== 'string') throw new Error('Stored session target has no session_id');
    return {
      type: 'existing_session',
      session_id: sessionId,
      instructions: typeof parsed.instructions === 'string' ? parsed.instructions : '',
    };
  }
  if (parsed.type === 'new_session') {
    return {
      type: 'new_session',
      environment: normalizeTargetEnvironment(parsed.environment as CreateSessionParams['environment']),
      input: parsed.input as CreateSessionParams['input'],
      ...(typeof parsed.agent_id === 'string' ? { agent_id: parsed.agent_id } : {}),
      ...(isRecord(parsed.agent) ? { agent: parsed.agent as CreateSessionParams['agent'] } : {}),
      ...(Array.isArray(parsed.vault_ids)
        ? { vault_ids: parsed.vault_ids as CreateSessionParams['vault_ids'] }
        : {}),
      ...(isRecord(parsed.metadata) ? { metadata: parsed.metadata as CreateSessionParams['metadata'] } : {}),
    };
  }
  throw new Error('Stored automation target has an unknown type');
}
