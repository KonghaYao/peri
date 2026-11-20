import { Hono } from 'hono';
import { platformError } from '../errors';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { sessionErrorResponse } from '../sessions/errors';
import { traceResources } from '../subagents/traces';

/** Routes for `/v1/agents/sessions/{session_id}/traces`. */
export function createSessionTraceRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    if (repos.sessions.get(sessionId) === null) {
      return platformError(c, 'not_found', 'Session not found', 404);
    }
    const request = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    // Rows are published traces only; an empty table answers an empty page, never an invented span.
    const traces = traceResources(repos.traces.listBySession(sessionId));
    const ordered = request.order === 'asc' ? traces : [...traces].reverse();
    return c.json(paginate(ordered, request, (trace) => trace.id));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
