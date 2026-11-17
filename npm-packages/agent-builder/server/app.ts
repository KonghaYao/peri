import type { Database } from 'bun:sqlite';
import { createApiErrorBody } from '@shared/api';
import { Hono } from 'hono';
import { AgentsRepository } from './agents/repository';
import { createAgentsRoutes } from './agents/routes';
import { createCapabilitiesRoutes } from './capabilities';
import { type ChatEngine, createChatEngine } from './chats/engine';
import { ChatsRepository } from './chats/repository';
import { createChatRoutes } from './chats/routes';
import { ChatService } from './chats/service';
import type { ServerEnv } from './env';
import { errorResponse } from './errors';
import { createHealthRoutes } from './health';
import { ProvidersRepository } from './providers/repository';
import { createProvidersRoutes } from './providers/routes';
import { ProviderService } from './providers/service';
import { SandboxCoordinator } from './sandbox/coordinator';
import { createOriginGuard } from './security';
import { mountStatic } from './static';

export interface AppDependencies {
  readonly env: ServerEnv;
  readonly db: Database;
  /** Optional so route tests can keep using an in-memory database without an engine. */
  readonly engine?: ChatEngine;
  readonly chatService?: ChatService;
  readonly sandbox?: SandboxCoordinator;
  /** Built SPA directory; null/undefined leaves the API-only shape used by tests and dev. */
  readonly webRoot?: string | null;
}

export function createApp(deps: AppDependencies): Hono {
  const app = new Hono();
  const agents = new AgentsRepository(deps.db);
  const providers = deps.chatService?.providers ?? new ProviderService(new ProvidersRepository(deps.db));
  // Callers that omit the engine get the configuration-only probe (route tests). Production passes
  // the Store-probed engine created in `main.ts`.
  const engine = deps.engine ?? createChatEngine(deps.env);
  const sandbox = deps.sandbox ?? new SandboxCoordinator(deps.env, null);
  const chatService =
    deps.chatService ??
    new ChatService({
      env: deps.env,
      engine,
      chats: new ChatsRepository(deps.db),
      agents,
      providers,
      sandbox,
    });

  app.use('*', createOriginGuard(deps.env.allowedOrigins));

  app.route(
    '/api/health',
    createHealthRoutes(async () => {
      // Health is also a readiness trigger: otherwise a Store outage is invisible until the next
      // chat request. `ensureReady` shares the capabilities TTL and probe budget.
      await engine.ensureReady?.();
      return engine.health();
    }),
  );
  app.route(
    '/api/capabilities',
    createCapabilitiesRoutes(
      providers,
      async () => {
        // A query is a recovery trigger too: capabilities must not stay frozen after boot.
        await engine.ensureReady?.();
        return engine.status;
      },
      sandbox.status,
    ),
  );
  app.route(
    '/api/agents',
    createAgentsRoutes(agents, deps.env, () => engine.status, sandbox.status, providers),
  );
  app.route('/api/providers', createProvidersRoutes(providers));
  app.route('/api/chats', createChatRoutes(chatService));

  app.notFound((c) => c.json(createApiErrorBody('not_found', 'Route not found'), 404));
  app.onError((error, c) => errorResponse(error, c));

  if (deps.webRoot !== null && deps.webRoot !== undefined) {
    mountStatic(app, deps.webRoot, String(Date.now()));
  }

  return app;
}
