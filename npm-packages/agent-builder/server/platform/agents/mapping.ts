import type { Database } from 'bun:sqlite';
import type { BareHarnessConfig } from '@peri-code/sdk';
import { McpServerDraft, SANDBOX_MCP_SERVER_NAME } from '@shared/agent';
import { ZodError } from 'zod';
import { ProvidersRepository } from '../../providers/repository';
import { PlatformNotImplementedError, PlatformRequestError } from '../errors';
import { parseJsonColumn, serializeJsonColumn, serializeMetadataColumn } from '../json';
import type { PlatformAgentRow } from '../repositories/agents';
import type { AgentToolParam, AgentUpdateParams, AgentWriteParams } from './schemas';

/** Downstream runtimes reject longer instructions even though the wire schema allows more. */
export const MAX_INSTRUCTIONS_BYTES = 65_536;
/** Compact UTF-8 JSON budget for the persisted tool list. */
export const MAX_TOOLS_BYTES = 3_145_728;
/** The stored name keeps an 80-character bound; the wire schema allows more. */
export const MAX_STORED_NAME_LENGTH = 80;

/** Mirrors the MCP server-name contract enforced by the shared agent config. */
const MCP_SERVER_LABEL = /^[A-Za-z0-9_-]{1,64}$/;

/** Capability keys are checked against the SDK config so a renamed key cannot be written blind. */
const CAPABILITY_KEY = {
  mcp: 'McpMiddleware',
  toolSearch: 'ToolSearch',
  webSearch: 'WebMiddleware',
  subAgent: 'SubAgentMiddleware',
  builtInSubagents: 'BuiltInSubagents',
} as const satisfies Record<string, keyof typeof BareHarnessConfig>;

export type PersistedAgentTool = { readonly type: string } & Record<string, unknown>;

/**
 * The MCP server entry persisted in `agents.config.mcpServers`. The documented stdio `cwd` /
 * `env_vars` and the HTTP `authorization` are kept here for the runtime; they are not part of
 * the credential-free tool resource.
 */
export type PersistedMcpServer = McpServerDraft & Record<string, unknown>;

export interface AgentWriteRecord {
  readonly name: string;
  readonly wireName: string | null;
  readonly config: string;
  readonly providerId: string | null;
  readonly model: string | null;
  readonly metadata: string;
  readonly tools: string;
  readonly multiAgent: string | null;
  readonly reasoning: string | null;
  readonly textConfig: string | null;
  readonly serviceTier: string | null;
  readonly compaction: string | null;
  readonly experimental: string | null;
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** Label and capability maps are user-keyed; a null prototype keeps `__proto__` an ordinary key. */
function emptyRecord<T>(): Record<string, T> {
  return Object.create(null) as Record<string, T>;
}

function assertInstructions(instructions: string | null): void {
  if (instructions === null) return;
  if (byteLength(instructions) > MAX_INSTRUCTIONS_BYTES) {
    throw new PlatformRequestError(
      `instructions must be at most ${MAX_INSTRUCTIONS_BYTES} bytes`,
      'instructions',
    );
  }
}

function truncateName(name: string): string {
  return Array.from(name).slice(0, MAX_STORED_NAME_LENGTH).join('');
}

function resolveModel(db: Database, model: string): { providerId: string | null; providerModel: string } {
  const separator = model.indexOf('/');
  if (separator <= 0 || separator === model.length - 1) {
    return { providerId: null, providerModel: model };
  }
  const prefix = model.slice(0, separator);
  const providerModel = model.slice(separator + 1);
  const provider = new ProvidersRepository(db)
    .list()
    .find((record) => record.id === prefix || record.name === prefix);
  if (provider === undefined) return { providerId: null, providerModel };
  return { providerId: provider.id, providerModel };
}

function toMcpServerDraft(tool: Extract<AgentToolParam, { type: 'mcp' }>, param: string): PersistedMcpServer {
  const transport = tool.transport;
  const candidate =
    transport.type === 'http'
      ? {
          type: 'http' as const,
          url: transport.server_url,
          ...(transport.headers === null || transport.headers === undefined
            ? {}
            : { headers: transport.headers }),
        }
      : {
          type: 'stdio' as const,
          command: transport.command,
          ...(transport.args === null || transport.args === undefined ? {} : { args: transport.args }),
          ...(transport.env === null || transport.env === undefined ? {} : { env: transport.env }),
        };
  let validated: McpServerDraft;
  try {
    validated = McpServerDraft.parse(candidate);
  } catch (error) {
    if (error instanceof ZodError) {
      throw new PlatformRequestError('MCP transport is not supported', param, error);
    }
    throw error;
  }
  if (transport.type === 'http') {
    return {
      ...validated,
      ...(transport.authorization === null || transport.authorization === undefined
        ? {}
        : { authorization: transport.authorization }),
    };
  }
  return {
    ...validated,
    cwd: transport.cwd,
    ...(transport.env_vars === null || transport.env_vars === undefined
      ? {}
      : { env_vars: [...transport.env_vars] }),
  };
}

function normalizeTool(tool: AgentToolParam, index: number): PersistedAgentTool {
  switch (tool.type) {
    case 'function':
    case 'computer_use':
      throw new PlatformNotImplementedError(
        `${tool.type} tools are not executed by this deployment`,
        `tools.${index}`,
      );
    case 'tool_search':
      return { type: 'tool_search' };
    case 'programmatic_tool_calling':
      return { type: 'programmatic_tool_calling', enabled: tool.enabled ?? true };
    case 'mcp': {
      const transport = tool.transport;
      return {
        type: 'mcp',
        server_label: tool.server_label,
        credential_id: tool.credential_id ?? null,
        transport:
          transport.type === 'http'
            ? { type: 'http', server_url: transport.server_url }
            : {
                type: 'stdio',
                command: transport.command,
                args: transport.args ?? [],
                cwd: transport.cwd,
                env_vars: transport.env_vars ?? [],
              },
        request_metadata: tool.request_metadata ?? {},
        allowed_tools: tool.allowed_tools ?? null,
        required: tool.required ?? false,
        connection_origin: tool.connection_origin ?? 'service',
      };
    }
    case 'web_search':
      return {
        type: 'web_search',
        mode: tool.mode ?? 'live',
        context_size: tool.context_size ?? 'medium',
        allowed_domains: tool.allowed_domains ?? null,
        location: tool.location ?? null,
      };
    case 'automations':
      return { type: 'automations' };
  }
}

function normalizeTools(tools: readonly AgentToolParam[]): PersistedAgentTool[] {
  return tools.map((tool, index) => normalizeTool(tool, index));
}

interface ToolMapping {
  readonly tools: PersistedAgentTool[];
  readonly metaHarness: Record<string, boolean>;
  readonly mcpServers: Record<string, PersistedMcpServer>;
}

/** Validates the tool list, persists its credential-free shape, and derives capabilities. */
function mapTools(tools: readonly AgentToolParam[]): ToolMapping {
  const persisted = normalizeTools(tools);
  if (byteLength(JSON.stringify(persisted)) > MAX_TOOLS_BYTES) {
    throw new PlatformRequestError(`tools must fit within ${MAX_TOOLS_BYTES} bytes of JSON`, 'tools');
  }

  const metaHarness = emptyRecord<boolean>();
  const mcpServers = emptyRecord<PersistedMcpServer>();
  for (const [index, tool] of tools.entries()) {
    const param = `tools.${index}`;
    switch (tool.type) {
      case 'mcp': {
        if (!MCP_SERVER_LABEL.test(tool.server_label)) {
          throw new PlatformRequestError(
            'MCP server labels may only contain letters, digits, "_" and "-" (max 64)',
            `${param}.server_label`,
          );
        }
        if (tool.server_label === SANDBOX_MCP_SERVER_NAME) {
          throw new PlatformRequestError(
            `"${SANDBOX_MCP_SERVER_NAME}" is reserved for the deployment sandbox`,
            `${param}.server_label`,
          );
        }
        mcpServers[tool.server_label] = toMcpServerDraft(tool, `${param}.transport`);
        metaHarness[CAPABILITY_KEY.mcp] = true;
        break;
      }
      case 'tool_search':
        metaHarness[CAPABILITY_KEY.toolSearch] = true;
        break;
      case 'web_search':
        metaHarness[CAPABILITY_KEY.webSearch] = true;
        break;
      default:
        break;
    }
  }
  return { tools: persisted, metaHarness, mcpServers };
}

export function buildAgentWrite(db: Database, params: AgentWriteParams): AgentWriteRecord {
  const instructions = params.instructions ?? null;
  assertInstructions(instructions);

  const mapping = mapTools(params.tools ?? []);
  const multiAgentEnabled = params.multi_agent?.enabled ?? false;
  mapping.metaHarness[CAPABILITY_KEY.subAgent] = multiAgentEnabled;
  mapping.metaHarness[CAPABILITY_KEY.builtInSubagents] = multiAgentEnabled;

  const resolved = resolveModel(db, params.model);
  const provider = resolved.providerId === null ? null : { model: resolved.providerModel };

  const config = {
    ...(instructions === null ? {} : { instructions }),
    provider,
    metaHarness: mapping.metaHarness,
    mcpServers: mapping.mcpServers,
  };

  const multiAgent = {
    enabled: multiAgentEnabled,
    max_concurrent_subagents: multiAgentEnabled ? (params.multi_agent?.max_concurrent_subagents ?? 6) : null,
  };
  const reasoning = {
    effort: params.reasoning?.effort ?? null,
    summary: params.reasoning?.summary ?? null,
  };
  const text = {
    format: params.text?.format ?? { type: 'text' },
    verbosity: params.text?.verbosity ?? 'medium',
  };
  const compaction = { async: params.compaction?.async ?? false };
  const experimental = { time_awareness: params.experimental?.time_awareness ?? false };

  return {
    name: truncateName(params.name ?? ''),
    wireName: params.name ?? null,
    config: serializeJsonColumn(config),
    providerId: resolved.providerId,
    model: params.model,
    metadata: serializeMetadataColumn(params.metadata ?? {}),
    tools: serializeJsonColumn(mapping.tools),
    multiAgent: serializeJsonColumn(multiAgent),
    reasoning: serializeJsonColumn(reasoning),
    textConfig: serializeJsonColumn(text),
    serviceTier: params.service_tier ?? 'auto',
    compaction: serializeJsonColumn(compaction),
    experimental: serializeJsonColumn(experimental),
  };
}

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function readPersistedTools(raw: string): PersistedAgentTool[] {
  const parsed = parseJsonColumn<unknown>(raw, []);
  if (!Array.isArray(parsed)) return [];
  return parsed.filter(
    (tool): tool is PersistedAgentTool =>
      typeof tool === 'object' && tool !== null && typeof (tool as { type?: unknown }).type === 'string',
  );
}

function readBooleanRecord(value: unknown): Record<string, boolean> {
  const record: Record<string, boolean> = {};
  for (const [key, item] of Object.entries(asRecord(value))) {
    if (typeof item === 'boolean') record[key] = item;
  }
  return record;
}

function readMcpServers(value: unknown): Record<string, PersistedMcpServer> {
  const servers: Record<string, PersistedMcpServer> = {};
  for (const [key, item] of Object.entries(asRecord(value))) {
    const record = asRecord(item);
    if (record.type === 'http' || record.type === 'stdio') {
      servers[key] = record as PersistedMcpServer;
    }
  }
  return servers;
}

function storedWireName(row: PlatformAgentRow): string | null {
  if (row.wire_name !== null) return row.wire_name.length === 0 ? null : row.wire_name;
  return row.name.length === 0 ? null : row.name;
}

/**
 * Patch semantics for `POST /agents/{id}`: omitted fields keep their stored value, supplied
 * fields replace it, and the documented nullable fields reset when `null` is supplied.
 */
export function buildAgentUpdate(
  db: Database,
  row: PlatformAgentRow,
  params: AgentUpdateParams,
): AgentWriteRecord {
  const config = asRecord(parseJsonColumn<unknown>(row.config, {}));
  const existingInstructions = typeof config.instructions === 'string' ? config.instructions : null;

  const mapping = params.tools === undefined ? null : mapTools(params.tools ?? []);
  const tools = mapping?.tools ?? readPersistedTools(row.tools);
  if (mapping === null && byteLength(JSON.stringify(tools)) > MAX_TOOLS_BYTES) {
    throw new PlatformRequestError(`tools must fit within ${MAX_TOOLS_BYTES} bytes of JSON`, 'tools');
  }

  const multiAgentEnabled =
    params.multi_agent === undefined
      ? asRecord(parseJsonColumn<unknown>(row.multi_agent, {})).enabled === true
      : (params.multi_agent?.enabled ?? false);
  const metaHarness = mapping?.metaHarness ?? readBooleanRecord(config.metaHarness);
  if (mapping !== null || params.multi_agent !== undefined) {
    metaHarness[CAPABILITY_KEY.subAgent] = multiAgentEnabled;
    metaHarness[CAPABILITY_KEY.builtInSubagents] = multiAgentEnabled;
  }
  const mcpServers = mapping?.mcpServers ?? readMcpServers(config.mcpServers);

  const instructions = params.instructions === undefined ? existingInstructions : params.instructions;
  assertInstructions(instructions);

  let providerId = row.provider_id;
  let provider: unknown = config.provider;
  let model = row.model;
  if (params.model !== undefined) {
    const resolved = resolveModel(db, params.model);
    providerId = resolved.providerId;
    provider = resolved.providerId === null ? null : { model: resolved.providerModel };
    model = params.model;
  }

  const storedConfig = {
    ...(instructions === null ? {} : { instructions }),
    provider,
    metaHarness,
    mcpServers,
  };

  const multiAgent = {
    enabled: multiAgentEnabled,
    max_concurrent_subagents:
      params.multi_agent === undefined
        ? asRecord(parseJsonColumn<unknown>(row.multi_agent, {})).max_concurrent_subagents
        : multiAgentEnabled
          ? (params.multi_agent?.max_concurrent_subagents ?? 6)
          : null,
  };
  const reasoning =
    params.reasoning === undefined
      ? row.reasoning
      : serializeJsonColumn({
          effort: params.reasoning?.effort ?? null,
          summary: params.reasoning?.summary ?? null,
        });
  const text =
    params.text === undefined
      ? row.text_config
      : serializeJsonColumn({
          format: params.text?.format ?? { type: 'text' },
          verbosity: params.text?.verbosity ?? 'medium',
        });
  const compaction =
    params.compaction === undefined
      ? row.compaction
      : serializeJsonColumn({ async: params.compaction?.async ?? false });
  const currentExperimental = asRecord(parseJsonColumn<unknown>(row.experimental, {}));
  const experimental =
    params.experimental === undefined || params.experimental === null
      ? row.experimental
      : serializeJsonColumn({
          time_awareness: params.experimental.time_awareness ?? currentExperimental.time_awareness === true,
        });

  const wireName = params.name === undefined ? storedWireName(row) : params.name;
  return {
    name: truncateName(wireName ?? ''),
    wireName,
    config: serializeJsonColumn(storedConfig),
    providerId,
    model,
    metadata: params.metadata === undefined ? row.metadata : serializeMetadataColumn(params.metadata ?? {}),
    tools: serializeJsonColumn(tools),
    multiAgent: params.multi_agent === undefined ? row.multi_agent : serializeJsonColumn(multiAgent),
    reasoning,
    textConfig: text,
    serviceTier: params.service_tier === undefined ? row.service_tier : (params.service_tier ?? 'auto'),
    compaction,
    experimental,
  };
}
