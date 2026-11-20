import { millisToUnixSeconds } from '../json';
import type { PlatformSubagentRow, PlatformSubagentStatus } from '../repositories/subagents';
import type { Snapshot } from '../sessions/diff';

/** Wire shapes and projection mapping for `/v1/agents/sessions/{session_id}/subagents`. */

export type SubagentTaskView = Snapshot['tasks'][number];

export type SubagentInstructionsWire = readonly unknown[] | null;

export interface SubagentResourceWire {
  readonly object: 'agent.session.subagent';
  readonly id: string;
  readonly session_id: string;
  readonly name: string | null;
  readonly instructions: SubagentInstructionsWire;
  readonly parent_agent_id: string;
  readonly status: PlatformSubagentStatus;
  readonly opened_at: number;
  readonly closed_at: number | null;
}

/** Task statuses that mean the subagent can no longer produce turns. */
const TERMINAL_TASK_STATUSES: ReadonlySet<string> = new Set(['completed', 'failed', 'cancelled']);

/** Subagent instance IDs and child thread IDs are the same value and must be real UUIDs. */
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isThreadUuid(value: string): boolean {
  return UUID_PATTERN.test(value);
}

/** Stored instructions are either a JSON content array or the plain task text. */
export function parseSubagentInstructions(raw: string | null): SubagentInstructionsWire {
  if (raw === null) return null;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return null;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (Array.isArray(parsed)) return parsed;
  } catch {
    // Plain text below; the stored column is not required to be JSON.
  }
  return [{ type: 'output_text', text: raw }];
}

export function serializeSubagent(row: PlatformSubagentRow): SubagentResourceWire {
  return {
    object: 'agent.session.subagent',
    id: row.id,
    session_id: row.session_id,
    name: row.name.length === 0 ? null : row.name,
    instructions: parseSubagentInstructions(row.instructions),
    parent_agent_id: row.parent_agent_id,
    status: row.status,
    opened_at: millisToUnixSeconds(row.opened_at),
    closed_at: row.closed_at === null ? null : millisToUnixSeconds(row.closed_at),
  };
}

/**
 * The subagent's child thread UUID from the parent projection, or null when this task is not a
 * subagent. Only UUID instances are adopted: the list resource has to answer with a child thread
 * ID, so anything else stays out instead of being synthesized.
 */
export function subagentTaskId(task: SubagentTaskView): string | null {
  if (task.kind !== 'subagent') return null;
  return isThreadUuid(task.taskId) ? task.taskId : null;
}

export function subagentTaskClosed(task: SubagentTaskView): boolean {
  return TERMINAL_TASK_STATUSES.has(task.status);
}

export function subagentTaskName(task: SubagentTaskView): string {
  const title = task.title.trim();
  return title.length === 0 ? 'Subagent' : title;
}

/** Projection timestamps are ISO strings; storage keeps Unix milliseconds. */
export function subagentTaskMillis(value: string | undefined): number | null {
  if (value === undefined || value.length === 0) return null;
  const millis = Date.parse(value);
  return Number.isFinite(millis) ? millis : null;
}
