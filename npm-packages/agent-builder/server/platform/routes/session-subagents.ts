import { type Context, Hono } from 'hono';
import { platformError } from '../errors';
import { type PageRequest, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { sessionErrorResponse } from '../sessions/errors';
import { StoreHistoryUnavailableError, StoreSchemaUnsupportedError } from '../subagents/history';
import { SubagentService } from '../subagents/service';

function pageRequest(c: Context): PageRequest {
  return parsePageRequest({
    limit: c.req.query('limit'),
    order: c.req.query('order'),
    after: c.req.query('after'),
  });
}

function schemaError(c: Context, error: StoreSchemaUnsupportedError): Response {
  return platformError(c, error.code, 'The Store schema does not support subagent history', 409);
}

/** A failed Store read is a server fault; answering an empty page would invent a result. */
async function history(c: Context, load: () => Promise<unknown>): Promise<Response> {
  try {
    return c.json(await load());
  } catch (error) {
    if (error instanceof StoreHistoryUnavailableError) {
      return platformError(c, error.code, 'Subagent history is unavailable', 503);
    }
    if (error instanceof StoreSchemaUnsupportedError) return schemaError(c, error);
    return sessionErrorResponse(error, c);
  }
}

/** Routes for `/v1/agents/sessions/{session_id}/subagents`. */
export function createSessionSubagentRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const service = new SubagentService(repos);

  routes.get('/', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    return c.json(service.list(sessionId, pageRequest(c)));
  });

  routes.get('/:subagent_id', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    return c.json(service.get(sessionId, subagentId));
  });

  routes.get('/:subagent_id/items', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const request = pageRequest(c);
    return history(c, () => service.items(sessionId, subagentId, request));
  });

  routes.get('/:subagent_id/turns', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const request = pageRequest(c);
    return history(c, () => service.turns(sessionId, subagentId, request));
  });

  routes.get('/:subagent_id/turns/:turn_id', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    return history(c, () => service.turn(sessionId, subagentId, turnId));
  });

  routes.get('/:subagent_id/turns/:turn_id/items', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    const request = pageRequest(c);
    return history(c, () => service.turnItems(sessionId, subagentId, turnId, request));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
