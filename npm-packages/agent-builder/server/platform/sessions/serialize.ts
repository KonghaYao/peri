import type { AgentDraft } from '@shared/agent';
import { millisToUnixSeconds, parseJsonColumn } from '../json';
import type { PlatformAgentRow, PlatformAgentsRepository } from '../repositories/agents';
import type { PlatformEnvironmentRow, PlatformEnvironmentsRepository } from '../repositories/environments';
import type {
  PlatformItemRow,
  PlatformSessionRow,
  PlatformSessionStatus,
  PlatformTurnRow,
} from '../repositories/sessions';

/** Wire shapes follow the documented session family; timestamps are Unix seconds. */

export interface TokenUsageWire {
  readonly input_tokens: number;
  readonly input_tokens_details: { readonly cached_tokens: number; readonly cache_write_tokens: number };
  readonly output_tokens: number;
  readonly output_tokens_details: { readonly reasoning_tokens: number };
  readonly total_tokens: number;
}

export interface AgentResourceWire {
  readonly id: string;
  readonly name: string | null;
  readonly model: string;
  readonly reasoning: unknown;
  readonly text: unknown;
  readonly service_tier: unknown;
  readonly instructions: string | null;
  readonly tools: readonly unknown[];
  readonly multi_agent: unknown;
  readonly compaction: unknown;
  readonly experimental: unknown;
}

export type EnvironmentResourceWire =
  | { readonly type: 'none' }
  | {
      readonly type: 'self_hosted';
      readonly remote_url: string;
      readonly id: string;
      readonly workspace_directory: string;
      readonly capability_directories: readonly string[];
    };

export type RequiredActionWire =
  | {
      readonly type: 'function_call';
      readonly turn_id: string;
      readonly call_id: string;
      readonly name: string;
      readonly arguments: unknown;
    }
  | {
      readonly type: 'mcp_approval_request';
      readonly turn_id: string;
      readonly request_id: string;
      readonly request: {
        readonly type: 'mcp_tool_call';
        readonly server_label: string;
        readonly name: string | null;
        readonly arguments: unknown;
        readonly message: string;
      };
    }
  | { readonly type: 'environment_connection'; readonly environment_id: string };

export interface SessionResourceWire {
  readonly object: 'agent.session';
  readonly id: string;
  readonly created_at: number;
  readonly last_active_at: number;
  readonly status: PlatformSessionStatus;
  readonly required_actions: readonly RequiredActionWire[];
  readonly error: string | null;
  readonly agent: AgentResourceWire;
  readonly environment: EnvironmentResourceWire;
  readonly metadata: Readonly<Record<string, string>>;
  readonly vault_ids: readonly string[];
  readonly usage: TokenUsageWire | null;
  readonly prompt_cache_options: unknown;
  readonly review: string | null;
}

export interface TurnResourceWire {
  readonly object: 'agent.session.turn';
  readonly id: string;
  readonly session_id: string;
  readonly agent_id: string;
  readonly subagent_id: string | null;
  readonly status: string;
  readonly created_at: number;
  readonly started_at: number | null;
  readonly completed_at: number | null;
  readonly error: { readonly code: string; readonly message: string } | null;
  readonly usage: TokenUsageWire | null;
  readonly prompt_cache_options: unknown;
}

export type SessionItemWire = Readonly<Record<string, unknown>> & {
  readonly id: string;
  readonly type: string;
};

/** `agent_config` keeps both the response snapshot and the resolved runtime draft. */
export interface SessionAgentSnapshot {
  readonly agent: AgentResourceWire;
  readonly draft: AgentDraft;
}

export function readAgentSnapshot(raw: string): SessionAgentSnapshot | null {
  const parsed = parseJsonColumn<Partial<SessionAgentSnapshot> | null>(raw, null);
  if (parsed === null || typeof parsed !== 'object') return null;
  if (parsed.agent === undefined || parsed.draft === undefined) return null;
  return { agent: parsed.agent, draft: parsed.draft };
}

export function serializeAgentSnapshot(snapshot: SessionAgentSnapshot): string {
  return JSON.stringify(snapshot);
}

/** Reads one nullable JSON column without throwing on legacy values. */
export function readJsonColumn(raw: string | null): unknown {
  if (raw === null) return null;
  return parseJsonColumn<unknown>(raw, null);
}

export function tokenUsage(input: {
  inputTokens: number;
  outputTokens: number;
  cachedTokens?: number;
  cacheWriteTokens?: number;
  reasoningTokens?: number;
}): TokenUsageWire {
  return {
    input_tokens: input.inputTokens,
    input_tokens_details: {
      cached_tokens: input.cachedTokens ?? 0,
      cache_write_tokens: input.cacheWriteTokens ?? 0,
    },
    output_tokens: input.outputTokens,
    output_tokens_details: { reasoning_tokens: input.reasoningTokens ?? 0 },
    total_tokens: input.inputTokens + input.outputTokens,
  };
}

export function readUsage(raw: string | null): TokenUsageWire | null {
  const parsed = parseJsonColumn<Partial<TokenUsageWire> | null>(raw, null);
  if (parsed === null || typeof parsed !== 'object') return null;
  if (typeof parsed.input_tokens !== 'number' || typeof parsed.output_tokens !== 'number') return null;
  const cached = parsed.input_tokens_details?.cached_tokens;
  const cacheWrite = parsed.input_tokens_details?.cache_write_tokens;
  const reasoning = parsed.output_tokens_details?.reasoning_tokens;
  return tokenUsage({
    inputTokens: parsed.input_tokens,
    outputTokens: parsed.output_tokens,
    ...(cached === undefined ? {} : { cachedTokens: cached }),
    ...(cacheWrite === undefined ? {} : { cacheWriteTokens: cacheWrite }),
    ...(reasoning === undefined ? {} : { reasoningTokens: reasoning }),
  });
}

/** Sums whatever token usage the projection exposes; unknown fields stay zero. */
export function sumEntryUsage(
  entries: readonly { tokenUsage?: Record<string, unknown> }[],
): TokenUsageWire | null {
  let input = 0;
  let output = 0;
  let seen = false;
  for (const entry of entries) {
    const usage = entry.tokenUsage;
    if (usage === undefined) continue;
    const nextInput = numberField(usage, ['inputTokens', 'input_tokens']);
    const nextOutput = numberField(usage, ['outputTokens', 'output_tokens']);
    if (nextInput === null && nextOutput === null) continue;
    seen = true;
    input += nextInput ?? 0;
    output += nextOutput ?? 0;
  }
  return seen ? tokenUsage({ inputTokens: input, outputTokens: output }) : null;
}

function numberField(source: Record<string, unknown>, keys: readonly string[]): number | null {
  for (const key of keys) {
    const value = source[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return null;
}

export function parseStringArray(raw: string | null): string[] {
  const parsed = parseJsonColumn<unknown>(raw, []);
  if (!Array.isArray(parsed)) return [];
  return parsed.filter((value): value is string => typeof value === 'string');
}

export function parseMetadata(raw: string | null): Record<string, string> {
  const parsed = parseJsonColumn<unknown>(raw, {});
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
  const metadata: Record<string, string> = {};
  for (const [key, value] of Object.entries(parsed)) {
    if (typeof value === 'string') metadata[key] = value;
  }
  return metadata;
}

export function parseRequiredActions(raw: string): RequiredActionWire[] {
  const parsed = parseJsonColumn<unknown>(raw, []);
  return Array.isArray(parsed) ? (parsed as RequiredActionWire[]) : [];
}

export function serializeTurn(row: PlatformTurnRow): TurnResourceWire {
  return {
    object: 'agent.session.turn',
    id: row.id,
    session_id: row.session_id,
    agent_id: row.agent_id,
    subagent_id: row.subagent_id,
    status: row.status,
    created_at: millisToUnixSeconds(row.created_at),
    started_at: row.started_at === null ? null : millisToUnixSeconds(row.started_at),
    completed_at: row.completed_at === null ? null : millisToUnixSeconds(row.completed_at),
    error:
      row.error_code === null ? null : { code: row.error_code, message: row.error_message ?? row.error_code },
    usage: readUsage(row.usage),
    prompt_cache_options: readJsonColumn(row.prompt_cache_options),
  };
}

export function serializeItem(row: PlatformItemRow): SessionItemWire {
  const payload = parseJsonColumn<Record<string, unknown>>(row.payload, {});
  return { ...payload, id: row.id, type: row.type, turn_id: row.turn_id };
}

export function serializeEnvironment(
  session: PlatformSessionRow,
  environments: PlatformEnvironmentsRepository,
): EnvironmentResourceWire {
  if (session.environment_type === 'none') return { type: 'none' };
  const environment = session.environment_id === null ? null : environments.get(session.environment_id);
  return selfHostedEnvironment(session, environment);
}

function selfHostedEnvironment(
  session: PlatformSessionRow,
  environment: PlatformEnvironmentRow | null,
): EnvironmentResourceWire {
  const config = parseJsonColumn<Record<string, unknown>>(environment?.config ?? '{}', {});
  const capabilityDirectories = config.capability_directories;
  return {
    type: 'self_hosted',
    remote_url: typeof config.remote_url === 'string' ? config.remote_url : '',
    id: environment?.id ?? session.environment_id ?? session.id,
    workspace_directory: typeof config.workspace_directory === 'string' ? config.workspace_directory : '',
    capability_directories: Array.isArray(capabilityDirectories)
      ? capabilityDirectories.filter((value): value is string => typeof value === 'string')
      : [],
  };
}

export function serializeSession(
  row: PlatformSessionRow,
  agents: PlatformAgentsRepository,
  environments: PlatformEnvironmentsRepository,
): SessionResourceWire {
  return {
    object: 'agent.session',
    id: row.id,
    created_at: millisToUnixSeconds(row.created_at),
    last_active_at: millisToUnixSeconds(row.last_active_at),
    status: row.status,
    required_actions: parseRequiredActions(row.required_actions),
    error: row.error,
    agent: agentResource(row, agents),
    environment: serializeEnvironment(row, environments),
    metadata: parseMetadata(row.metadata),
    vault_ids: parseStringArray(row.vault_ids),
    usage: readUsage(row.usage),
    prompt_cache_options: readJsonColumn(row.prompt_cache_options),
    review: row.review,
  };
}

function agentResource(row: PlatformSessionRow, agents: PlatformAgentsRepository): AgentResourceWire {
  const snapshot = readAgentSnapshot(row.agent_config);
  if (snapshot !== null) return snapshot.agent;
  const agent = agents.get(row.agent_id);
  return fallbackAgentResource(row.agent_id, agent);
}

/** Legacy rows without a snapshot still answer with the seeded agent shape. */
export function fallbackAgentResource(agentId: string, agent: PlatformAgentRow | null): AgentResourceWire {
  return {
    id: agentId,
    name: agent?.name ?? null,
    model: agent?.model ?? '',
    reasoning: readJsonColumn(agent?.reasoning ?? null),
    text: readJsonColumn(agent?.text_config ?? null),
    service_tier: readJsonColumn(agent?.service_tier ?? null),
    instructions: readJsonColumn(agent?.config ?? null) === null ? null : instructionsOf(agent),
    tools: parseJsonColumn<unknown[]>(agent?.tools ?? null, []),
    multi_agent: readJsonColumn(agent?.multi_agent ?? null),
    compaction: readJsonColumn(agent?.compaction ?? null),
    experimental: readJsonColumn(agent?.experimental ?? null),
  };
}

function instructionsOf(agent: PlatformAgentRow | null): string | null {
  if (agent === null) return null;
  const config = parseJsonColumn<Record<string, unknown>>(agent.config, {});
  return typeof config.instructions === 'string' ? config.instructions : null;
}
