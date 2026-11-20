import { BareHarnessConfig, CHAT_SEND_TEXT_MAX_LENGTH } from '@peri-code/sdk';
import type { AgentCapabilities } from '@shared/agent';
import type { ChatRuntimeStatus, SandboxStatus } from '@shared/chat';
import { Hono } from 'hono';
import type { ProviderService } from './providers/service';

/**
 * Runtime snapshot taken after `ensureReady()`. `status` is the configured mode; `ready` is the
 * engine's observed health, so `wasm` with a failed/starting engine stays `ready:false`.
 */
export interface ChatRuntimeView {
  readonly status: ChatRuntimeStatus;
  readonly ready: boolean;
}

/** 基础设施状态与 SQLite 凭据存在状态分离；均不代表模型已认证。 */
export function buildCapabilities(
  providers: ProviderService,
  runtime: ChatRuntimeView,
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
    chatRuntime: runtime.status.mode,
    chatRuntimeReady: runtime.ready,
    chatRuntimeReasons: [...runtime.status.reasons],
    // History is only persisted when the engine can actually run, not merely configured.
    historyPersisted: runtime.ready,
    sandbox,
  };
}

export type RuntimeResolver = () => ChatRuntimeView | Promise<ChatRuntimeView>;

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
