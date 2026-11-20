import { newPlatformId } from '../ids';
import type { PlatformEventRow } from '../repositories/sessions';
import type {
  RequiredActionWire,
  SessionItemWire,
  SessionResourceWire,
  TokenUsageWire,
  TurnResourceWire,
} from './serialize';

/** Event payloads use the documented `type` discriminator, which is also the SSE event name. */
export type SessionEventData = Readonly<Record<string, unknown>> & {
  readonly type: string;
  readonly event_id: string;
};

export function newEventId(): string {
  return newPlatformId();
}

export function sessionEvent(
  type:
    | 'agent.session.created'
    | 'agent.session.idle'
    | 'agent.session.in_progress'
    | 'agent.session.requires_action'
    | 'agent.session.failed',
  eventId: string,
  session: SessionResourceWire,
): SessionEventData {
  return { type, event_id: eventId, session };
}

export function turnEvent(
  type: 'agent.session.turn.created' | 'agent.session.turn.in_progress' | 'agent.session.turn.completed',
  eventId: string,
  turn: TurnResourceWire,
): SessionEventData {
  return {
    type,
    event_id: eventId,
    session_id: turn.session_id,
    turn_id: turn.id,
    turn,
  };
}

export function turnTerminalEvent(
  type: 'agent.session.turn.completed' | 'agent.session.turn.failed' | 'agent.session.turn.cancelled',
  eventId: string,
  turn: TurnResourceWire,
  usage: TokenUsageWire | null,
): SessionEventData {
  return {
    type,
    event_id: eventId,
    session_id: turn.session_id,
    turn_id: turn.id,
    turn,
    usage,
  };
}

export function itemEvent(
  type: 'agent.session.turn.item.added' | 'agent.session.turn.item.done',
  eventId: string,
  sessionId: string,
  turnId: string,
  outputIndex: number,
  item: SessionItemWire,
): SessionEventData {
  return {
    type,
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    output_index: outputIndex,
    item,
  };
}

export function contentPartEvent(
  type: 'agent.session.turn.content_part.added' | 'agent.session.turn.content_part.done',
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  contentIndex: number,
  text: string,
): SessionEventData {
  return {
    type,
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    content_index: contentIndex,
    part: { type: 'output_text', text },
  };
}

export function outputTextDeltaEvent(
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  contentIndex: number,
  delta: string,
): SessionEventData {
  return {
    type: 'agent.session.turn.output_text.delta',
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    content_index: contentIndex,
    delta,
  };
}

export function outputTextDoneEvent(
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  contentIndex: number,
  text: string,
): SessionEventData {
  return {
    type: 'agent.session.turn.output_text.done',
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    content_index: contentIndex,
    text,
  };
}

export function reasoningPartEvent(
  type: 'agent.session.turn.reasoning_summary_part.added' | 'agent.session.turn.reasoning_summary_part.done',
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  summaryIndex: number,
  text: string,
  status?: 'incomplete',
): SessionEventData {
  return {
    type,
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    summary_index: summaryIndex,
    part: { type: 'summary_text', text },
    ...(type === 'agent.session.turn.reasoning_summary_part.done' ? { status: status ?? null } : {}),
  };
}

export function reasoningTextDeltaEvent(
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  summaryIndex: number,
  delta: string,
): SessionEventData {
  return {
    type: 'agent.session.turn.reasoning_summary_text.delta',
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    summary_index: summaryIndex,
    delta,
  };
}

export function reasoningTextDoneEvent(
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  summaryIndex: number,
  text: string,
): SessionEventData {
  return {
    type: 'agent.session.turn.reasoning_summary_text.done',
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    summary_index: summaryIndex,
    text,
  };
}

export function commandOutputDeltaEvent(
  eventId: string,
  sessionId: string,
  turnId: string,
  itemId: string,
  outputIndex: number,
  delta: string,
): SessionEventData {
  return {
    type: 'agent.output.command_execution_output.delta',
    event_id: eventId,
    session_id: sessionId,
    turn_id: turnId,
    item_id: itemId,
    output_index: outputIndex,
    delta,
  };
}

export interface SubagentWire {
  readonly id: string;
  readonly object: 'agent.session.subagent';
  readonly session_id: string;
  readonly name: string | null;
  readonly instructions: readonly unknown[] | null;
  readonly parent_agent_id: string;
  readonly status: string;
  readonly opened_at: number;
  readonly closed_at: number | null;
}

export function subagentEvent(
  type: 'agent.session.subagent.created' | 'agent.session.subagent.active' | 'agent.session.subagent.closed',
  eventId: string,
  subagent: SubagentWire,
): SessionEventData {
  return { type, event_id: eventId, subagent };
}

/** One SSE frame: `id` carries the log position so `Last-Event-ID` resumes exactly. */
export function formatEventFrame(row: PlatformEventRow): string {
  return `id: ${row.seq}\nevent: ${row.type}\ndata: ${row.data}\n\n`;
}

export function requiredActionsFromInteractions(
  interactions: readonly {
    readonly id: string;
    readonly kind: string;
    readonly turnId?: string;
    readonly toolCallId?: string;
    readonly title?: string;
    readonly message?: string;
  }[],
): RequiredActionWire[] {
  return interactions.map((interaction) => {
    if (interaction.kind === 'permission') {
      return {
        type: 'function_call',
        turn_id: interaction.turnId ?? '',
        call_id: interaction.toolCallId ?? interaction.id,
        name: interaction.title ?? 'tool',
        arguments: {},
      } satisfies RequiredActionWire;
    }
    return {
      type: 'mcp_approval_request',
      turn_id: interaction.turnId ?? '',
      request_id: interaction.id,
      request: {
        type: 'mcp_tool_call',
        server_label: '',
        name: interaction.title ?? null,
        arguments: {},
        message: interaction.message ?? '',
      },
    } satisfies RequiredActionWire;
  });
}
