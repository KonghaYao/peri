import { type Context, Hono } from 'hono';
import { platformError } from '../errors';
import { type PageRequest, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { sessionErrorResponse } from '../sessions/errors';
import { StoreHistoryUnavailableError, StoreSchemaUnsupportedError } from '../subagents/history';
import { emptyPage, SUBAGENT_HISTORY_DEGRADED, SubagentService } from '../subagents/service';

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

/** Lists degrade to an empty page; the header keeps the missing history visible to callers. */
async function historyPage(c: Context, load: () => Promise<unknown>): Promise<Response> {
  try {
    return c.json(await load());
  } catch (error) {
    if (error instanceof StoreHistoryUnavailableError) {
      c.header('x-platform-degraded', SUBAGENT_HISTORY_DEGRADED);
      return c.json(emptyPage());
    }
    if (error instanceof StoreSchemaUnsupportedError) return schemaError(c, error);
    return sessionErrorResponse(error, c);
  }
}

/** A single turn cannot degrade to "not found": an unreadable Store answers 503 explicitly. */
async function historyValue(c: Context, load: () => Promise<unknown>): Promise<Response> {
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
    return historyPage(c, () => service.items(sessionId, subagentId, request));
  });

  routes.get('/:subagent_id/turns', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const request = pageRequest(c);
    return historyPage(c, () => service.turns(sessionId, subagentId, request));
  });

  routes.get('/:subagent_id/turns/:turn_id', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    return historyValue(c, () => service.turn(sessionId, subagentId, turnId));
  });

  routes.get('/:subagent_id/turns/:turn_id/items', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    const subagentId = c.req.param('subagent_id') ?? '';
    const turnId = c.req.param('turn_id') ?? '';
    const request = pageRequest(c);
    return historyPage(c, () => service.turnItems(sessionId, subagentId, turnId, request));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
