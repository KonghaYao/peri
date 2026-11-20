import type { Database } from 'bun:sqlite';
import { createApiErrorBody } from '@shared/api';
import { Hono } from 'hono';
import type { ChatEngine } from './chats/engine';
import type { ChatService } from './chats/service';
import type { ServerEnv } from './env';
import { errorResponse } from './errors';
import { createPlatformRoutes } from './platform/routes';
import type { SandboxCoordinator } from './sandbox/coordinator';
import { createOriginGuard } from './security';

export interface AppDependencies {
  readonly env: ServerEnv;
  readonly db: Database;
  /**
   * Kept for callers that install the session runtime against an in-memory database. `/v1`
   * resolves its own services; these fields are not used by the HTTP composition itself.
   */
  readonly engine?: ChatEngine;
  readonly chatService?: ChatService;
  readonly sandbox?: SandboxCoordinator;
}

export function createApp(deps: AppDependencies): Hono {
  const app = new Hono();

  app.use('*', createOriginGuard(deps.env.allowedOrigins));

  app.route('/v1', createPlatformRoutes({ db: deps.db }));

  app.notFound((c) => c.json(createApiErrorBody('not_found', 'Route not found'), 404));
  app.onError((error, c) => errorResponse(error, c));

  return app;
}
