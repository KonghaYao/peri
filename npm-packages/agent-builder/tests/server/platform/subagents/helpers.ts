import type { Harness } from '../sessions/helpers';
import { tick } from '../sessions/helpers';

/** Shared fixtures for the subagent/artifact/trace resource family. */

export interface SubagentTaskEvent {
  readonly instanceId: string;
  readonly name?: string;
  /** Omitted means the subagent is still running. */
  readonly status?: 'running' | 'completed' | 'failed' | 'cancelled';
}

/**
 * Feeds one `peri/agent_event` projection fact into the parent chat. This is the same channel the
 * runtime uses for `subagent_started` / `subagent_stopped`; the recorder reads the resulting tasks.
 */
export async function emitSubagentTask(
  harness: Harness,
  sessionId: string,
  event: SubagentTaskEvent,
): Promise<void> {
  const stopped = event.status !== undefined && event.status !== 'running';
  const value: Record<string, unknown> = {
    instance_id: event.instanceId,
    agent_name: event.name ?? 'researcher',
    is_background: false,
  };
  if (stopped) value.is_error = event.status === 'failed';
  harness.actor(sessionId).emit({
    kind: 'notice',
    notification: {
      jsonrpc: '2.0',
      method: 'peri/agent_event',
      params: {
        event_json: JSON.stringify({
          type: stopped ? 'subagent_stopped' : 'subagent_started',
          value,
        }),
      },
    },
  } as never);
  await tick();
}

export function newChildThreadId(): string {
  return crypto.randomUUID();
}

export function seedSubagent(
  harness: Harness,
  sessionId: string,
  overrides: Partial<{
    id: string;
    name: string;
    status: 'active' | 'closed';
    openedAt: number;
    instructions: string;
  }> = {},
) {
  return harness.repos.subagents.create({
    id: overrides.id ?? newChildThreadId(),
    sessionId,
    name: overrides.name ?? 'researcher',
    parentAgentId: 'agent-parent',
    status: overrides.status ?? 'active',
    ...(overrides.openedAt === undefined ? {} : { openedAt: overrides.openedAt }),
    ...(overrides.instructions === undefined ? {} : { instructions: overrides.instructions }),
  });
}

let turnSequence = 0;

export function seedTurn(
  harness: Harness,
  sessionId: string,
  subagentId: string | null,
  overrides: Partial<{
    status: 'queued' | 'in_progress' | 'waiting' | 'completed' | 'failed' | 'cancelled';
  }> = {},
) {
  turnSequence += 1;
  return harness.repos.turns.create({
    sessionId,
    agentId: 'agent-parent',
    subagentId,
    runtimeTurnId: `runtime-turn-${turnSequence}`,
    bindingRevision: 1,
    status: overrides.status ?? 'completed',
  });
}

export function seedItem(
  harness: Harness,
  sessionId: string,
  turnId: string,
  outputIndex: number,
  text: string,
) {
  return harness.repos.items.create({
    sessionId,
    turnId,
    outputIndex,
    type: 'message',
    status: 'completed',
    payload: JSON.stringify({ type: 'message', role: 'assistant', text }),
  });
}
