import type {
  AgentDraft,
  AgentPreviewResponse,
  AgentPreviewTarget,
  McpServerDraft,
  PreviewProvider,
  PreviewWarning,
} from '@shared/agent';
import { isReferenceProvider, PREVIEW_CHAT_ID, SANDBOX_MCP_SERVER_NAME } from '@shared/agent';
import type { ChatRuntimeStatus, SandboxStatus } from '@shared/chat';
import type { ServerEnv } from '../env';
import { AppError } from '../errors';
import type { ProviderService, ResolvedProvider } from '../providers/service';
import type { SandboxInjection } from '../sandbox/coordinator';

const DEFAULT_RUNTIME: ChatRuntimeStatus = { mode: 'not_configured', reasons: [] };
const DEFAULT_SANDBOX: SandboxStatus = { mode: 'disabled', reasons: [] };

/** Every credential-bearing value is replaced by this exact string in preview output. */
export const REDACTED_VALUE = '[redacted]';

type TargetEnv = Pick<ServerEnv, 'workspaceUri' | 'storeUrl' | 'storeToken'>;

export function resolveAgentProvider(draft: AgentDraft, service?: ProviderService): ResolvedProvider | null {
  const provider = draft.config.provider;
  if (!provider) return null;
  if (service) return service.resolve(provider);
  if (isReferenceProvider(provider)) throw new AppError('not_found', 'Provider resolver is required');
  return { provider: structuredClone(provider), apiKey: undefined, providerId: null };
}

/**
 * Validates sandbox prerequisites before any holder is acquired. The same checks run again in the
 * target composer, so a stored legacy row cannot bypass them.
 */
export function assertSandboxTargetCompatible(draft: AgentDraft): void {
  if (SANDBOX_MCP_SERVER_NAME in draft.config.mcpServers) {
    throw new AppError('invalid_input', `"${SANDBOX_MCP_SERVER_NAME}" is reserved for the sandbox`, {
      details: { server: SANDBOX_MCP_SERVER_NAME },
    });
  }
  if (draft.config.metaHarness.McpMiddleware === false) {
    throw new AppError('not_ready', 'Sandbox requires MCP capability, but McpMiddleware is off', {
      details: { reason: 'sandbox_mcp_disabled' },
      retryable: false,
    });
  }
}

export interface BuildTargetOptions {
  /** Deployment-owned sandbox MCP target; absent for previews and disabled sandboxes. */
  readonly sandbox?: SandboxInjection | null;
  readonly resolved?: ResolvedProvider | null;
  readonly providers?: ProviderService;
}

/**
 * Composes exactly the target the chat actor expects: the PeriConfig lives under
 * `agent.settings.config`; `cwd`, `storage` and `machineId` are Manager-scoped and must not appear
 * in `agent`. `workspace` is the canonical URI, never two path fields. The sandbox MCP entry is
 * deployment state: its URL and credentials never leave this function's return value.
 */
export function buildAgentTarget(
  env: TargetEnv,
  draft: AgentDraft,
  chatId: string,
  options: BuildTargetOptions = {},
): AgentPreviewTarget {
  const resolved =
    options.resolved === undefined ? resolveAgentProvider(draft, options.providers) : options.resolved;
  const provider = resolved?.provider;
  const apiKey = resolved?.apiKey;
  const providers: PreviewProvider[] = provider
    ? [
        {
          id: provider.id,
          type: provider.type,
          ...(provider.baseUrl === undefined ? {} : { baseUrl: provider.baseUrl }),
          ...(apiKey === undefined ? {} : { apiKey }),
        },
      ]
    : [];
  const profiles = provider ? { sonnet: { provider: provider.id, model: provider.model } } : {};
  const metaHarness = { ...draft.config.metaHarness };
  const mcpServers: Record<string, McpServerDraft> = structuredClone(draft.config.mcpServers);

  if (SANDBOX_MCP_SERVER_NAME in mcpServers) {
    // A legacy row may predate the reserved name; never overwrite or forward it silently.
    assertSandboxTargetCompatible(draft);
  }

  const target: AgentPreviewTarget = {
    chatId,
    workspace: env.workspaceUri,
    agent: {
      settings: {
        config: {
          active_alias: 'sonnet',
          providers,
          profiles,
          ...(Object.keys(metaHarness).length === 0 ? {} : { meta_harness: metaHarness }),
        },
      },
    },
    storage: {
      url: env.storeUrl,
      ...(env.storeToken === undefined ? {} : { authToken: env.storeToken }),
    },
    session: {
      ...(draft.config.instructions.length === 0 ? {} : { instructions: draft.config.instructions }),
      ...(Object.keys(mcpServers).length === 0 ? {} : { mcpServers }),
    },
  };
  if (options.sandbox) return injectSandboxTarget(target, draft, options.sandbox);
  return target;
}

/** 冻结完成后只合成 deployment injection，不再读取 Provider 或凭据。 */
export function injectSandboxTarget(
  target: AgentPreviewTarget,
  draft: AgentDraft,
  sandbox: SandboxInjection,
): AgentPreviewTarget {
  assertSandboxTargetCompatible(draft);
  const next = structuredClone(target);
  next.agent.settings.config.meta_harness = {
    ...next.agent.settings.config.meta_harness,
    McpMiddleware: true,
  };
  next.session.mcpServers = {
    ...next.session.mcpServers,
    [sandbox.serverName]: { type: 'http', url: sandbox.url, headers: { ...sandbox.headers } },
  };
  return next;
}

function redactRecord(record: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.keys(record).map((key) => [key, REDACTED_VALUE]));
}

function redactMcpServer(server: McpServerDraft): McpServerDraft {
  if (server.type === 'http') {
    return {
      type: 'http',
      url: server.url,
      ...(server.headers === undefined ? {} : { headers: redactRecord(server.headers) }),
    };
  }
  return {
    type: 'stdio',
    command: server.command,
    ...(server.args === undefined ? {} : { args: [...server.args] }),
    ...(server.env === undefined ? {} : { env: redactRecord(server.env) }),
  };
}

/**
 * Copies the target while masking credentials. Values (not keys) are dropped so the preview can
 * show which headers/env vars are configured without exposing them.
 */
export function redactTarget(target: AgentPreviewTarget): AgentPreviewTarget {
  const config = target.agent.settings.config;
  return {
    chatId: target.chatId,
    workspace: target.workspace,
    agent: {
      settings: {
        config: {
          active_alias: 'sonnet',
          providers: config.providers.map((provider) => ({
            ...provider,
            ...(provider.apiKey === undefined ? {} : { apiKey: REDACTED_VALUE }),
          })),
          profiles: Object.fromEntries(
            Object.entries(config.profiles).map(([alias, profile]) => [alias, { ...profile }]),
          ),
          ...(config.meta_harness === undefined ? {} : { meta_harness: { ...config.meta_harness } }),
        },
      },
    },
    storage: {
      ...target.storage,
      ...(target.storage.authToken === undefined ? {} : { authToken: REDACTED_VALUE }),
    },
    session: {
      ...(target.session.instructions === undefined ? {} : { instructions: target.session.instructions }),
      ...(target.session.mcpServers === undefined
        ? {}
        : {
            mcpServers: Object.fromEntries(
              Object.entries(target.session.mcpServers).map(([name, server]) => [
                name,
                redactMcpServer(server),
              ]),
            ),
          }),
    },
  };
}

export function buildPreviewWarnings(
  draft: AgentDraft,
  runtime: ChatRuntimeStatus = DEFAULT_RUNTIME,
  sandbox: SandboxStatus = DEFAULT_SANDBOX,
  resolved: ResolvedProvider | null = resolveAgentProvider(draft),
): PreviewWarning[] {
  const warnings: PreviewWarning[] = [];
  const provider = draft.config.provider;

  if (!provider || !isReferenceProvider(provider)) {
    warnings.push({
      code: 'provider_missing',
      message:
        '请设置 PERI_PROVIDER_TYPE 与 PERI_PROVIDER_API_KEY 后重启以同步 Provider；旧内嵌配置仅可编辑和预览。',
    });
  } else if (resolved?.apiKey === undefined) {
    warnings.push({
      code: 'api_key_missing',
      message:
        '此 Provider 待填写 API key；请设置 PERI_PROVIDER_API_KEY 后重启。凭据存在不表示已通过模型认证。',
    });
  }

  if (runtime.mode !== 'wasm' && Object.keys(draft.config.mcpServers).length > 0) {
    warnings.push({
      code: 'mcp_not_connected',
      message: '真实运行时未配置，MCP 服务器不会连接；预览只验证配置。',
    });
  }

  if (runtime.mode !== 'wasm') {
    warnings.push({
      code: 'history_not_persisted',
      message: '未配置真实 Store，会话历史不会持久化；重启后历史为空。',
    });
  }

  if (sandbox.mode === 'misconfigured') {
    warnings.push({
      code: 'sandbox_unavailable',
      message: '已启用 Sandbox，但部署配置不完整；创建对话会被拒绝，请运行 doctor 查看原因。',
    });
  } else if (sandbox.mode === 'ready' && draft.config.metaHarness.McpMiddleware === false) {
    warnings.push({
      code: 'sandbox_mcp_disabled',
      message: 'McpMiddleware 被显式关闭；该 Agent 无法使用 Sandbox，创建对话会被拒绝。',
    });
  }

  return warnings;
}

/** Preview never touches storage: it composes and redacts the target, then reports warnings. */
export function createPreviewResponse(
  env: TargetEnv,
  draft: AgentDraft,
  runtime: ChatRuntimeStatus = DEFAULT_RUNTIME,
  sandbox: SandboxStatus = DEFAULT_SANDBOX,
  providers?: ProviderService,
): AgentPreviewResponse {
  const resolved = resolveAgentProvider(draft, providers);
  return {
    target: redactTarget(buildAgentTarget(env, draft, PREVIEW_CHAT_ID, { resolved })),
    warnings: buildPreviewWarnings(draft, runtime, sandbox, resolved),
  };
}
