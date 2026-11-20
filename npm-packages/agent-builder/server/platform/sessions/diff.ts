import type { ChatStateSnapshot } from '@peri-code/sdk/view';
import { type TokenUsageWire, tokenUsage } from './serialize';

export type Snapshot = ChatStateSnapshot;
export type Entry = Snapshot['entries'][number];
export type Block = Entry['blocks'][number];
export type Tool = Snapshot['toolCalls'][number];
export type TurnView = Snapshot['turns'][number];

export type ItemStatus = 'in_progress' | 'completed' | 'incomplete';

export interface PartState {
  readonly key: string;
  readonly index: number;
  text: string;
  open: boolean;
}

export interface ItemState {
  readonly id: string;
  readonly type: string;
  readonly sourceKey: string;
  readonly outputIndex: number;
  /** Adopted from pre-existing history: rows are written but no events are replayed. */
  readonly silent: boolean;
  status: ItemStatus;
  readonly payload: Record<string, unknown>;
  readonly parts: Map<string, PartState>;
  done: boolean;
}

export interface TurnState {
  readonly id: string;
  readonly runtimeTurnId: string;
  status: string;
  announced: boolean;
  terminal: boolean;
  nextOutputIndex: number;
  readonly items: Map<string, ItemState>;
  readonly entryUsage: Map<string, TokenUsageWire>;
}

export function isTerminalTurn(view: TurnView): boolean {
  if (view.completedAt !== null) return true;
  return (
    view.status === 'completed' ||
    view.status === 'failed' ||
    view.status === 'cancelled' ||
    view.status === 'error'
  );
}

export function isLiveTurn(view: TurnView): boolean {
  return view.startedAt !== null && !isTerminalTurn(view);
}

/** Wire turn status from the projection's exit status. */
export function mapTurnStatus(status: string): 'completed' | 'failed' | 'cancelled' {
  if (status === 'completed') return 'completed';
  if (status === 'cancelled') return 'cancelled';
  return 'failed';
}

export function entryStatus(entry: Entry): ItemStatus {
  if (entry.status === 'completed') return 'completed';
  if (entry.status === 'cancelled' || entry.status === 'error' || entry.status === 'failed')
    return 'incomplete';
  return 'in_progress';
}

export function toolStatus(tool: Tool): ItemStatus {
  if (tool.status === 'completed') return 'completed';
  if (tool.status === 'failed' || tool.status === 'cancelled' || tool.status === 'error') return 'incomplete';
  return 'in_progress';
}

export function isTerminalEntry(entry: Entry): boolean {
  return entryStatus(entry) !== 'in_progress';
}

export function isTerminalTool(tool: Tool): boolean {
  return toolStatus(tool) !== 'in_progress';
}

/** ACP tool kinds are transport-neutral; only `execute` maps to a command execution item. */
export function itemTypeForTool(tool: Tool): 'command_execution' | 'function_call' {
  return tool.kind === 'execute' ? 'command_execution' : 'function_call';
}

export function textOf(entry: Entry): string {
  let text = '';
  for (const block of entry.blocks) if (block.type === 'text') text += block.text;
  return text;
}

export function toolPayload(tool: Tool, type: string): Record<string, unknown> {
  if (type === 'command_execution') {
    return {
      type,
      command: commandOf(tool),
      cwd: cwdOf(tool),
      status: toolStatus(tool),
      output: commandOutputOf(tool),
      exit_code: exitCodeOf(tool),
      duration_ms: durationOf(tool),
    };
  }
  return {
    type,
    call_id: tool.toolCallId,
    name: tool.name,
    arguments: tool.arguments ?? {},
    status: toolStatus(tool),
  };
}

function commandOf(tool: Tool): string {
  const args = asRecord(tool.arguments);
  const command = args.command ?? args.cmd;
  if (typeof command === 'string') return command;
  if (Array.isArray(command)) return command.filter((part) => typeof part === 'string').join(' ');
  return tool.name;
}

function cwdOf(tool: Tool): string | null {
  const cwd = asRecord(tool.arguments).cwd;
  return typeof cwd === 'string' ? cwd : null;
}

export function commandOutputOf(tool: Tool): string {
  if (typeof tool.result === 'string') return tool.result;
  const record = asRecord(tool.result);
  for (const key of ['output', 'stdout', 'text']) {
    if (typeof record[key] === 'string') return record[key] as string;
  }
  return '';
}

function exitCodeOf(tool: Tool): number | null {
  const record = asRecord(tool.result);
  for (const key of ['exit_code', 'exitCode']) {
    if (typeof record[key] === 'number') return record[key] as number;
  }
  return null;
}

function durationOf(tool: Tool): number | null {
  const started = Date.parse(tool.createdAt ?? '');
  const completed = Date.parse(tool.completedAt ?? '');
  if (Number.isNaN(started) || Number.isNaN(completed)) return null;
  return Math.max(0, completed - started);
}

export function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

export function millisOf(iso: string | null): number | null {
  if (iso === null) return null;
  const parsed = Date.parse(iso);
  return Number.isNaN(parsed) ? null : parsed;
}

/** Sums per-entry usage once per entry id; repeated snapshots do not double count. */
export function accumulateUsage(turn: TurnState, entry: Entry): void {
  if (entry.tokenUsage === undefined) {
    turn.entryUsage.delete(entry.entryId);
    return;
  }
  const input = numberField(entry.tokenUsage, ['inputTokens', 'input_tokens']);
  const output = numberField(entry.tokenUsage, ['outputTokens', 'output_tokens']);
  if (input === null && output === null) {
    turn.entryUsage.delete(entry.entryId);
    return;
  }
  turn.entryUsage.set(entry.entryId, tokenUsage({ inputTokens: input ?? 0, outputTokens: output ?? 0 }));
}

function numberField(source: Record<string, unknown>, keys: readonly string[]): number | null {
  for (const key of keys) {
    const value = source[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return null;
}

export function sumTurnUsage(turn: TurnState): TokenUsageWire | null {
  if (turn.entryUsage.size === 0) return null;
  let input = 0;
  let output = 0;
  for (const usage of turn.entryUsage.values()) {
    input += usage.input_tokens;
    output += usage.output_tokens;
  }
  return tokenUsage({ inputTokens: input, outputTokens: output });
}

/** Internal identity keys never cross the wire (the serializer strips `_`-prefixed keys). */
export function sourceKeyOf(payload: Record<string, unknown>, callId: string | null): string | null {
  if (typeof payload._entry_id === 'string') {
    return typeof payload._block_id === 'string'
      ? `reason:${payload._entry_id}:${payload._block_id}`
      : `entry:${payload._entry_id}`;
  }
  const toolId = payload._call_id ?? callId;
  return typeof toolId === 'string' ? `tool:${toolId}` : null;
}

export function stripInternal(payload: Record<string, unknown>): Record<string, unknown> {
  const wire: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(payload)) {
    if (key.startsWith('_')) continue;
    wire[key] = value;
  }
  return wire;
}
