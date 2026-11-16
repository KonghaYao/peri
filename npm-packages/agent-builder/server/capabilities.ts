import { BareHarnessConfig, CHAT_SEND_TEXT_MAX_LENGTH } from '@peri-code/sdk';
import type { AgentCapabilities } from '@shared/agent';
import type { ChatRuntimeStatus, SandboxStatus } from '@shared/chat';
import { Hono } from 'hono';
import type { ProviderService } from './providers/service';

/** 基础设施状态与 SQLite 凭据存在状态分离；均不代表模型已认证。 */
export function buildCapabilities(
  providers: ProviderService,
  runtime: ChatRuntimeStatus,
  sandbox: SandboxStatus,
): AgentCapabilities {
  const configured = providers.list().filter((provider) => provider.hasCredential);
  return {
    metaHarnessKeys: Object.keys(BareHarnessConfig),
    sendTextMaxLength: CHAT_SEND_TEXT_MAX_LENGTH,
    providers: [
      { type: 'anthropic', configured: configured.some((provider) => provider.type === 'anthropic') },
      { type: 'openai', configured: configured.some((provider) => provider.type === 'openai') },
    ],
    chatRuntime: runtime.mode,
    chatRuntimeReasons: [...runtime.reasons],
    historyPersisted: runtime.mode === 'wasm',
    sandbox,
  };
}

export type RuntimeResolver = () => ChatRuntimeStatus | Promise<ChatRuntimeStatus>;

export function createCapabilitiesRoutes(
  providers: ProviderService,
  resolveRuntime: RuntimeResolver,
  sandbox: SandboxStatus,
): Hono {
  const routes = new Hono();

  // Resolving inside the handler lets a recoverable runtime report the Store state after probing.
  routes.get('/', async (c) => c.json(buildCapabilities(providers, await resolveRuntime(), sandbox)));

  return routes;
}
