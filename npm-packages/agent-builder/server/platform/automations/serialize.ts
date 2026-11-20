import { millisToUnixSeconds, parseJsonColumn, parseMetadataColumn } from '../json';
import type {
  PlatformAutomationInvocationRow,
  PlatformAutomationRow,
  PlatformAutomationStatus,
} from '../repositories/automations';
import type { CreateSessionParams } from '../sessions/params';
import { parseStoredTarget, type StoredAutomationTarget } from './schemas';

export type CreatedByResource =
  | { readonly type: 'developer' }
  | {
      readonly type: 'agent';
      readonly session_id: string;
      readonly tool_call_id: string;
    };

export type AutomationTargetResource =
  | {
      readonly type: 'new_session';
      readonly agent_id: string | null;
      readonly agent: unknown;
      readonly environment: unknown;
      readonly input: readonly unknown[];
      readonly vault_ids: readonly string[];
      readonly metadata: Readonly<Record<string, string>>;
    }
  | {
      readonly type: 'existing_session';
      readonly session_id: string;
      readonly instructions: string;
    };

export interface AutomationResourceBase {
  readonly id: string;
  readonly object: 'agent.automation';
  readonly created_at: number;
  readonly name: string;
  readonly target: AutomationTargetResource;
  readonly metadata: Readonly<Record<string, string>>;
  readonly created_by: CreatedByResource;
  readonly pause_reason: string | null;
}

export interface PeriodicAutomationResource extends AutomationResourceBase {
  readonly type: 'periodic';
  readonly expression: string;
  readonly timezone: string;
  readonly status: Extract<PlatformAutomationStatus, 'active' | 'paused'>;
}

export interface OnceAutomationResource extends AutomationResourceBase {
  readonly type: 'once';
  readonly run_at: number;
  readonly status: PlatformAutomationStatus;
}

export type AutomationResource = PeriodicAutomationResource | OnceAutomationResource;

export interface AutomationInvocationResource {
  readonly id: string;
  readonly object: 'agent.automation.invocation';
  readonly automation_id: string;
  readonly created_at: number;
  readonly source:
    | { readonly type: 'manual' }
    | {
        readonly type: 'automation';
        readonly automation: AutomationResource;
      };
  readonly scheduled_at: number | null;
  readonly session_id: string | null;
  readonly status: PlatformAutomationInvocationRow['status'];
  readonly skip_reason: 'pending_delivery' | null;
  readonly error: { readonly code: string; readonly message: string } | null;
}

function parseCreatedBy(raw: string): CreatedByResource {
  const parsed = parseJsonColumn<Record<string, unknown>>(raw, { type: 'developer' });
  if (
    parsed.type === 'agent' &&
    typeof parsed.session_id === 'string' &&
    typeof parsed.tool_call_id === 'string'
  ) {
    return { type: 'agent', session_id: parsed.session_id, tool_call_id: parsed.tool_call_id };
  }
  return { type: 'developer' };
}

function serializeTarget(target: StoredAutomationTarget): AutomationTargetResource {
  if (target.type === 'existing_session') {
    return {
      type: 'existing_session',
      session_id: target.session_id,
      instructions: target.instructions,
    };
  }
  return {
    type: 'new_session',
    agent_id: target.agent_id ?? null,
    agent: target.agent ?? null,
    environment: serializeEnvironment(target.environment),
    input: Array.isArray(target.input) ? target.input : [],
    vault_ids: target.vault_ids ?? [],
    metadata: target.metadata ?? {},
  };
}

/**
 * The automation contract's environment resource is narrower than the session parameter: it always
 * carries the resolved reference fields. Rows written by earlier versions may still hold the
 * retired `local` type, which is reported as the documented self-hosted shape.
 */
function serializeEnvironment(environment: CreateSessionParams['environment']): unknown {
  const stored = environment as {
    readonly type: string;
    readonly workspace_directory?: string;
    readonly capability_directories?: readonly string[] | null;
  };
  if (stored.type === 'self_hosted' || stored.type === 'local') {
    return {
      type: 'self_hosted',
      environment_id: null,
      workspace_directory: stored.workspace_directory ?? '',
      capability_directories: stored.capability_directories ?? [],
    };
  }
  return { type: 'none' };
}

export function serializeAutomation(row: PlatformAutomationRow): AutomationResource {
  const base: AutomationResourceBase = {
    id: row.id,
    object: 'agent.automation',
    created_at: millisToUnixSeconds(row.created_at),
    name: row.name,
    target: serializeTarget(parseStoredTarget(row.target)),
    metadata: parseMetadataColumn(row.metadata),
    created_by: parseCreatedBy(row.created_by),
    pause_reason: row.pause_reason,
  };
  if (row.type === 'periodic') {
    return {
      ...base,
      type: 'periodic',
      expression: row.expression ?? '',
      timezone: row.timezone ?? '',
      status: row.status === 'active' ? 'active' : 'paused',
    };
  }
  return {
    ...base,
    type: 'once',
    run_at: millisToUnixSeconds(row.run_at ?? 0),
    status: row.status,
  };
}

/**
 * `skip_reason` is narrowed by the documented wire contract to `pending_delivery` or null; the
 * internal reason (overlap, missed, target_session_failed) stays on the stored row for operators.
 */
function serializeSkipReason(row: PlatformAutomationInvocationRow): 'pending_delivery' | null {
  if (row.status !== 'skipped' || row.skip_reason === null) return null;
  return 'pending_delivery';
}

export function serializeInvocation(
  row: PlatformAutomationInvocationRow,
  automation: PlatformAutomationRow,
): AutomationInvocationResource {
  return {
    id: row.id,
    object: 'agent.automation.invocation',
    automation_id: row.automation_id,
    created_at: millisToUnixSeconds(row.created_at),
    source:
      row.source === 'manual'
        ? { type: 'manual' }
        : { type: 'automation', automation: serializeAutomation(automation) },
    scheduled_at: row.scheduled_at === null ? null : millisToUnixSeconds(row.scheduled_at),
    session_id: row.session_id,
    status: row.status,
    skip_reason: serializeSkipReason(row),
    error: row.error_code === null ? null : { code: row.error_code, message: row.error_message ?? '' },
  };
}
