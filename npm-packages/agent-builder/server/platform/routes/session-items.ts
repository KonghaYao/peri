import { Hono } from 'hono';
import { notImplemented } from '../errors';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { sessionErrorResponse } from '../sessions/errors';
import { sessionRuntimeFor } from '../sessions/runtime';
import { SessionService } from '../sessions/service';

/** Routes for `/v1/agents/sessions/{session_id}/items`. */
export function createSessionItemRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const installed = sessionRuntimeFor(repos);
    const sessionId = c.req.param('session_id') ?? '';
    if (installed === null) return notImplemented(c, 'session item list', [sessionId]);
    const page = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    return c.json(new SessionService(installed).items(sessionId, page));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
