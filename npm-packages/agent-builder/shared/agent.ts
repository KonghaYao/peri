import { z } from 'zod';
import { SandboxStatus } from './chat';

/** Reserved name for the deployment-injected workspace MCP; users cannot declare it. */
export const SANDBOX_MCP_SERVER_NAME = 'peri-workspace-sandbox';

const McpServerName = z
  .string()
  .regex(/^[A-Za-z0-9_-]{1,64}$/, 'MCP server names may only contain letters, digits, "_" and "-"');

/** MCP over HTTP cannot carry file/ftp URIs; the SDK would fail at connect time. */
const McpHttpUrl = z.url({
  protocol: /^https?$/,
  error: 'MCP server URLs must use http or https',
});

export const McpHttpServerDraft = z.strictObject({
  type: z.literal('http'),
  url: McpHttpUrl,
  headers: z.record(z.string(), z.string()).optional(),
});

export const McpStdioServerDraft = z.strictObject({
  type: z.literal('stdio'),
  command: z.string().trim().min(1, 'MCP stdio commands cannot be blank'),
  args: z.array(z.string()).optional(),
  env: z.record(z.string(), z.string()).optional(),
});

/** Only declarative transports can be persisted; the SDK's ACP transport needs a live instance. */
export const McpServerDraft = z.discriminatedUnion('type', [McpHttpServerDraft, McpStdioServerDraft]);
export type McpServerDraft = z.infer<typeof McpServerDraft>;

export const InlineAgentProvider = z.strictObject({
  id: z.string().min(1).max(64),
  type: z.enum(['anthropic', 'openai']),
  model: z.string().min(1).max(200),
  baseUrl: z.url().optional(),
});
export type InlineAgentProvider = z.infer<typeof InlineAgentProvider>;
export const ReferenceAgentProvider = z.strictObject({
  kind: z.literal('reference'),
  providerId: z.uuid(),
  model: z.string().min(1).max(200),
});
export const AgentProvider = z.union([ReferenceAgentProvider, InlineAgentProvider]);
export type AgentProvider = z.infer<typeof AgentProvider>;
export function isReferenceProvider(
  provider: AgentProvider,
): provider is z.infer<typeof ReferenceAgentProvider> {
  return 'kind' in provider;
}

/**
 * Strict on purpose: unknown keys are rejected so a field that looks editable
 * (for example `allowedTools`) can never be silently ignored by the SDK.
 */
export const AgentConfig = z.strictObject({
  instructions: z.string().max(32_768).default(''),
  provider: AgentProvider.nullable().default(null),
  metaHarness: z.record(z.string(), z.boolean()).default({}),
  mcpServers: z.record(McpServerName, McpServerDraft).default({}),
});
export type AgentConfig = z.output<typeof AgentConfig>;
export type AgentConfigInput = z.input<typeof AgentConfig>;

export const AgentDraft = z
  .strictObject({
    name: z.string().trim().min(1).max(80),
    description: z.string().trim().max(400).default(''),
    config: AgentConfig,
  })
  .superRefine((draft, ctx) => {
    if (SANDBOX_MCP_SERVER_NAME in draft.config.mcpServers) {
      ctx.addIssue({
        code: 'custom',
        path: ['config', 'mcpServers', SANDBOX_MCP_SERVER_NAME],
        message: `"${SANDBOX_MCP_SERVER_NAME}" is reserved for the deployment sandbox`,
      });
    }
  });
export type AgentDraft = z.output<typeof AgentDraft>;
export type AgentDraftInput = z.input<typeof AgentDraft>;

export const AgentSummary = z.strictObject({
  id: z.uuid(),
  name: z.string().min(1).max(80),
  description: z.string().max(400),
  createdAt: z.number().int().nonnegative(),
  updatedAt: z.number().int().nonnegative(),
  revision: z.number().int().positive(),
});
export type AgentSummary = z.infer<typeof AgentSummary>;

export const Agent = z.strictObject({
  ...AgentSummary.shape,
  config: AgentConfig,
});
export type Agent = z.infer<typeof Agent>;

export const AgentListResponse = z.strictObject({ agents: z.array(AgentSummary) });
export type AgentListResponse = z.infer<typeof AgentListResponse>;

export const AgentResponse = z.strictObject({ agent: Agent });
export type AgentResponse = z.infer<typeof AgentResponse>;

export const ProviderCapability = z.strictObject({
  type: z.enum(['anthropic', 'openai']),
  /** 同协议至少一条共享 SQLite Provider 存有 key；不表示特定 Provider 可用或已认证。 */
  configured: z.boolean(),
});
export type ProviderCapability = z.infer<typeof ProviderCapability>;

export const AgentCapabilities = z.strictObject({
  metaHarnessKeys: z.array(z.string().min(1)),
  sendTextMaxLength: z.number().int().positive(),
  providers: z.array(ProviderCapability),
  chatRuntime: z.enum(['wasm', 'not_configured']),
  /** Codes, not secrets: the UI turns these into actionable hints. */
  chatRuntimeReasons: z.array(z.string().min(1)),
  historyPersisted: z.boolean(),
  sandbox: SandboxStatus,
});
export type AgentCapabilities = z.infer<typeof AgentCapabilities>;

/** Placeholder chat id used by `/api/agents/preview`; never persisted or opened. */
export const PREVIEW_CHAT_ID = 'preview';

export const PreviewProvider = z.strictObject({
  id: z.string().min(1),
  type: z.enum(['anthropic', 'openai']),
  apiKey: z.string().optional(),
  baseUrl: z.string().optional(),
});
export type PreviewProvider = z.infer<typeof PreviewProvider>;

/**
 * The target an Agent would be opened with. Preview returns the same shape with every credential
 * replaced by `[redacted]`; `agent` deliberately has no `cwd`/`storage`/`machineId`.
 */
export const AgentPreviewTarget = z.strictObject({
  chatId: z.string().min(1),
  /** Canonical `peri+sandbox://` workspace URI; the deployment is the only source. */
  workspace: z.string().min(1),
  agent: z.strictObject({
    settings: z.strictObject({
      config: z.strictObject({
        active_alias: z.literal('sonnet'),
        providers: z.array(PreviewProvider),
        profiles: z.record(
          z.string(),
          z.strictObject({ provider: z.string().min(1), model: z.string().min(1) }),
        ),
        meta_harness: z.record(z.string(), z.boolean()).optional(),
      }),
    }),
  }),
  storage: z.strictObject({ url: z.string().min(1), authToken: z.string().optional() }),
  session: z.strictObject({
    instructions: z.string().optional(),
    mcpServers: z.record(z.string(), McpServerDraft).optional(),
  }),
});
export type AgentPreviewTarget = z.infer<typeof AgentPreviewTarget>;

export const PREVIEW_WARNING_CODES = [
  'provider_missing',
  'api_key_missing',
  'mcp_not_connected',
  'history_not_persisted',
  'sandbox_mcp_disabled',
  'sandbox_unavailable',
] as const;
export const PreviewWarning = z.strictObject({
  code: z.enum(PREVIEW_WARNING_CODES),
  message: z.string().min(1),
});
export type PreviewWarning = z.infer<typeof PreviewWarning>;

export const AgentPreviewResponse = z.strictObject({
  target: AgentPreviewTarget,
  warnings: z.array(PreviewWarning),
});
export type AgentPreviewResponse = z.infer<typeof AgentPreviewResponse>;
