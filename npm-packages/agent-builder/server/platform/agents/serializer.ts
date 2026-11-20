import { millisToUnixSeconds, parseJsonColumn, parseMetadataColumn } from '../json';
import type { PlatformAgentRow } from '../repositories/agents';
import type { PersistedAgentTool } from './mapping';

const VERBOSITY = new Set(['low', 'medium', 'high']);
const SERVICE_TIERS = new Set(['auto', 'default', 'flex', 'priority', 'fast', 'ultrafast']);

export interface AgentReasoningResource {
  readonly effort: string | null;
  readonly summary: string | null;
}

export interface AgentTextResource {
  readonly format:
    | { readonly type: 'text' }
    | { readonly type: 'json_schema'; readonly schema: Record<string, unknown> };
  readonly verbosity: 'low' | 'medium' | 'high';
}

export interface AgentMultiAgentResource {
  readonly enabled: boolean;
  readonly max_concurrent_subagents: number | null;
}

export interface AgentResourceBody {
  readonly id: string;
  readonly object: 'agent';
  readonly name: string | null;
  readonly instructions: string | null;
  readonly model: string;
  readonly metadata: Record<string, string>;
  readonly reasoning: AgentReasoningResource;
  readonly text: AgentTextResource;
  readonly tools: PersistedAgentTool[];
  readonly multi_agent: AgentMultiAgentResource;
  readonly compaction: { readonly async: boolean };
  readonly experimental: { readonly time_awareness: boolean };
  readonly service_tier: string;
  readonly created_at: number;
  readonly updated_at: number;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function readInstructions(config: Record<string, unknown>): string | null {
  return typeof config.instructions === 'string' ? config.instructions : null;
}

function readTools(raw: string): PersistedAgentTool[] {
  const parsed = parseJsonColumn<unknown>(raw, []);
  if (!Array.isArray(parsed)) return [];
  return parsed.filter(
    (tool): tool is PersistedAgentTool =>
      typeof tool === 'object' && tool !== null && typeof (tool as { type?: unknown }).type === 'string',
  );
}

function readReasoning(raw: string | null): AgentReasoningResource {
  const value = asRecord(parseJsonColumn<unknown>(raw, {}));
  return {
    effort: typeof value?.effort === 'string' ? value.effort : null,
    summary: typeof value?.summary === 'string' ? value.summary : null,
  };
}

function readText(raw: string | null): AgentTextResource {
  const value = asRecord(parseJsonColumn<unknown>(raw, {}));
  const format = asRecord(value?.format);
  const verbosity = value?.verbosity;
  const resolvedFormat: AgentTextResource['format'] =
    format?.type === 'json_schema' && asRecord(format.schema) !== null
      ? { type: 'json_schema', schema: asRecord(format.schema) ?? {} }
      : { type: 'text' };
  return {
    format: resolvedFormat,
    verbosity:
      typeof verbosity === 'string' && VERBOSITY.has(verbosity)
        ? (verbosity as AgentTextResource['verbosity'])
        : 'medium',
  };
}

function readMultiAgent(raw: string | null): AgentMultiAgentResource {
  const value = asRecord(parseJsonColumn<unknown>(raw, {}));
  const enabled = value?.enabled === true;
  const maximum = value?.max_concurrent_subagents;
  return {
    enabled,
    max_concurrent_subagents:
      enabled && typeof maximum === 'number' && Number.isInteger(maximum) ? maximum : null,
  };
}

function readCompaction(raw: string | null): { async: boolean } {
  const value = asRecord(parseJsonColumn<unknown>(raw, {}));
  return { async: value?.async === true };
}

function readExperimental(raw: string | null): { time_awareness: boolean } {
  const value = asRecord(parseJsonColumn<unknown>(raw, {}));
  return { time_awareness: value?.time_awareness === true };
}

function readServiceTier(raw: string | null): string {
  return raw !== null && SERVICE_TIERS.has(raw) ? raw : 'auto';
}

/**
 * Platform rows keep the requested model string in `model`. Legacy rows created before the
 * `/v1` surface only carry it inside `config.provider`, so fall back to that value.
 */
function readModel(row: PlatformAgentRow, config: Record<string, unknown>): string {
  if (row.model !== null) return row.model;
  const provider = asRecord(config.provider);
  return typeof provider?.model === 'string' ? provider.model : '';
}

/** The wire name keeps the full documented value; `agents.name` remains the 80-character stored view. */
function wireName(row: PlatformAgentRow): string | null {
  const name = row.wire_name ?? row.name;
  return name.length === 0 ? null : name;
}

/** Serializes a stored row into the documented agent resource. */
export function serializeAgent(row: PlatformAgentRow): AgentResourceBody {
  const config = asRecord(parseJsonColumn<unknown>(row.config, {})) ?? {};
  return {
    id: row.id,
    object: 'agent',
    name: wireName(row),
    instructions: readInstructions(config),
    model: readModel(row, config),
    metadata: parseMetadataColumn(row.metadata),
    reasoning: readReasoning(row.reasoning),
    text: readText(row.text_config),
    tools: readTools(row.tools),
    multi_agent: readMultiAgent(row.multi_agent),
    compaction: readCompaction(row.compaction),
    experimental: readExperimental(row.experimental),
    service_tier: readServiceTier(row.service_tier),
    created_at: millisToUnixSeconds(row.created_at),
    updated_at: millisToUnixSeconds(row.updated_at),
  };
}
