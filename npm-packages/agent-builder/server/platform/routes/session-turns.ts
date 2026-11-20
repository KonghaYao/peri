import { type Context, Hono } from 'hono';
import { notImplemented } from '../errors';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { sessionErrorResponse } from '../sessions/errors';
import { sessionRuntimeFor } from '../sessions/runtime';
import { SessionService } from '../sessions/service';

/** Routes for `/v1/agents/sessions/{session_id}/turns`. */
export function createSessionTurnRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const readPage = (c: Context) =>
    parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });

  routes.get('/', (c) => {
    const installed = sessionRuntimeFor(repos);
    const sessionId = c.req.param('session_id') ?? '';
    if (installed === null) return notImplemented(c, 'session turn list', [sessionId]);
    return c.json(new SessionService(installed).turns(sessionId, readPage(c)));
  });

  routes.get('/:turn_id', (c) => {
    const installed = sessionRuntimeFor(repos);
    const sessionId = c.req.param('session_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    if (installed === null) return notImplemented(c, 'session turn', [sessionId, turnId]);
    return c.json(new SessionService(installed).turn(sessionId, turnId));
  });

  routes.get('/:turn_id/items', (c) => {
    const installed = sessionRuntimeFor(repos);
    const sessionId = c.req.param('session_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    if (installed === null) return notImplemented(c, 'session turn item list', [sessionId, turnId]);
    return c.json(new SessionService(installed).turnItems(sessionId, turnId, readPage(c)));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
