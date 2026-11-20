import { z } from 'zod';

/**
 * Wire schemas for the reusable-agent resource.
 *
 * Every object is strict: a field this deployment does not implement must fail the request
 * instead of being silently dropped. Optional nulls follow the documented nullability so a
 * client can distinguish "leave unset" from an explicit empty value.
 */

const MAX_MODEL_LENGTH = 1_048_576;
const MAX_TEXT_LENGTH = 1_048_576;
const MAX_TOOL_ITEMS = 2_000;
const MAX_NAME_LENGTH = 128;
const MAX_METADATA_PAIRS = 16;

const Metadata = z
  .record(z.string().min(1).max(64), z.string().max(512))
  .refine((value) => Object.keys(value).length <= MAX_METADATA_PAIRS, {
    message: 'metadata supports at most 16 key-value pairs',
  });

export const ReasoningEffort = z.enum(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
export const ReasoningSummary = z.enum(['concise', 'detailed', 'auto']);

const ReasoningParam = z
  .strictObject({
    effort: ReasoningEffort.nullable().optional(),
    summary: ReasoningSummary.nullable().optional(),
  })
  .nullable();

const TextFormatParam = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('text') }),
  z.strictObject({
    type: z.literal('json_schema'),
    schema: z.record(z.string(), z.unknown()),
  }),
]);

const TextParam = z
  .strictObject({
    format: TextFormatParam.nullable().optional(),
    verbosity: z.enum(['low', 'medium', 'high']).nullable().optional(),
  })
  .nullable();

const ServiceTierParam = z
  .enum(['auto', 'default', 'flex', 'priority', 'fast', 'ultrafast'])
  .nullable()
  .optional();

const MultiAgentParam = z
  .strictObject({
    enabled: z.boolean(),
    max_concurrent_subagents: z.number().int().min(1).max(4_294_967_295).optional(),
  })
  .nullable();

const CompactionParam = z
  .strictObject({
    async: z.boolean().nullable().optional(),
  })
  .nullable();

const ExperimentalParam = z
  .strictObject({
    time_awareness: z.boolean().optional(),
  })
  .nullable();

const McpHttpTransportParam = z.strictObject({
  type: z.literal('http'),
  server_url: z.string().max(MAX_TEXT_LENGTH),
  authorization: z.string().max(MAX_TEXT_LENGTH).nullable().optional(),
  headers: z.record(z.string().min(1).max(256), z.string().max(MAX_TEXT_LENGTH)).nullable().optional(),
});

const McpStdioTransportParam = z.strictObject({
  type: z.literal('stdio'),
  command: z.string().max(MAX_TEXT_LENGTH),
  args: z.array(z.string().max(MAX_TEXT_LENGTH)).max(16_384).nullable().optional(),
  cwd: z.string().max(MAX_TEXT_LENGTH),
  env: z.record(z.string().min(1).max(256), z.string().max(MAX_TEXT_LENGTH)).nullable().optional(),
  env_vars: z.array(z.string().max(MAX_TEXT_LENGTH)).max(16_384).nullable().optional(),
});

const McpTransportParam = z.discriminatedUnion('type', [McpHttpTransportParam, McpStdioTransportParam]);

const WebSearchLocationParam = z.strictObject({
  country: z.string().max(MAX_TEXT_LENGTH).nullable(),
  region: z.string().max(MAX_TEXT_LENGTH).nullable(),
  city: z.string().max(MAX_TEXT_LENGTH).nullable(),
  timezone: z.string().max(MAX_TEXT_LENGTH).nullable(),
});

const FunctionToolParam = z.strictObject({
  type: z.literal('function'),
  name: z.string().max(MAX_TEXT_LENGTH),
  description: z.string().max(MAX_TEXT_LENGTH),
  parameters: z
    .record(z.string().min(1).max(256), z.unknown())
    .refine((value) => Object.keys(value).length <= 1_024, {
      message: 'function parameters support at most 1024 properties',
    }),
  defer_loading: z.boolean().optional(),
  async: z.boolean().optional(),
});

const ToolSearchParam = z.strictObject({ type: z.literal('tool_search') });

const ProgrammaticToolCallingParam = z.strictObject({
  type: z.literal('programmatic_tool_calling'),
  enabled: z.boolean().optional(),
});

const McpToolParam = z.strictObject({
  type: z.literal('mcp'),
  server_label: z.string().max(MAX_TEXT_LENGTH),
  credential_id: z.string().max(MAX_TEXT_LENGTH).nullable().optional(),
  transport: McpTransportParam,
  request_metadata: z
    .record(z.string().min(1).max(256), z.unknown())
    .refine((value) => Object.keys(value).length <= 1_024, {
      message: 'MCP request metadata supports at most 1024 properties',
    })
    .nullable()
    .optional(),
  allowed_tools: z.array(z.string().max(MAX_TEXT_LENGTH)).max(16_384).nullable().optional(),
  required: z.boolean().optional(),
  connection_origin: z.enum(['service', 'environment']).nullable().optional(),
});

const WebSearchToolParam = z.strictObject({
  type: z.literal('web_search'),
  mode: z.enum(['disabled', 'cached', 'live']).nullable().optional(),
  context_size: z.enum(['low', 'medium', 'high']).nullable().optional(),
  allowed_domains: z.array(z.string().max(MAX_TEXT_LENGTH)).max(16_384).nullable().optional(),
  location: WebSearchLocationParam.nullable().optional(),
});

const ComputerUseToolParam = z.strictObject({
  type: z.literal('computer_use'),
  include_screenshots: z.boolean().optional(),
});

const AutomationsToolParam = z.strictObject({ type: z.literal('automations') });

export const AgentToolParam = z.discriminatedUnion('type', [
  FunctionToolParam,
  ToolSearchParam,
  ProgrammaticToolCallingParam,
  McpToolParam,
  WebSearchToolParam,
  ComputerUseToolParam,
  AutomationsToolParam,
]);

export const ToolsParam = z.array(AgentToolParam).max(MAX_TOOL_ITEMS).nullable();

/**
 * Create requires `model` and writes the documented defaults. Update is a patch: every field is
 * optional, omission keeps the stored value, and the documented nullable fields reset when the
 * caller supplies `null`.
 */
export const AgentWriteParams = z.strictObject({
  name: z.string().max(MAX_NAME_LENGTH).nullable().optional(),
  model: z.string().max(MAX_MODEL_LENGTH),
  instructions: z.string().max(MAX_TEXT_LENGTH).nullable().optional(),
  metadata: Metadata.nullable().optional(),
  tools: ToolsParam.optional(),
  multi_agent: MultiAgentParam.optional(),
  reasoning: ReasoningParam.optional(),
  text: TextParam.optional(),
  service_tier: ServiceTierParam,
  compaction: CompactionParam.optional(),
  experimental: ExperimentalParam.optional(),
});

export const AgentUpdateParams = z.strictObject({
  name: z.string().max(MAX_NAME_LENGTH).nullable().optional(),
  model: z.string().max(MAX_MODEL_LENGTH).optional(),
  instructions: z.string().max(MAX_TEXT_LENGTH).nullable().optional(),
  metadata: Metadata.nullable().optional(),
  tools: ToolsParam.optional(),
  multi_agent: MultiAgentParam.optional(),
  reasoning: ReasoningParam.optional(),
  text: TextParam.optional(),
  service_tier: ServiceTierParam,
  compaction: CompactionParam.optional(),
  experimental: ExperimentalParam.optional(),
});

export type AgentWriteParams = z.infer<typeof AgentWriteParams>;
export type AgentUpdateParams = z.infer<typeof AgentUpdateParams>;
export type AgentToolParam = z.infer<typeof AgentToolParam>;
