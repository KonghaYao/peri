import { Hono } from 'hono';
import { AutomationHttpError, automationErrorResponse } from '../automations/errors';
import { automationRuntimeFor } from '../automations/runtime';
import { CreateAutomationParams, UpdateAutomationParams } from '../automations/schemas';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';

const MAX_FILTER_LENGTH = 1_048_576;

/** Routes for `/v1/agents/automations`. */
export function createAutomationRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const service = () => automationRuntimeFor(repos).service;

  routes.get('/', (c) => {
    const page = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    const sessionId = c.req.query('session_id');
    if (sessionId !== undefined && sessionId.length > MAX_FILTER_LENGTH) {
      throw new AutomationHttpError('invalid_input', 'session_id is too long', 400, 'session_id');
    }
    return c.json(service().list(page, sessionId ?? null));
  });

  routes.post('/', async (c) => {
    const params = await parsePlatformJson(c, CreateAutomationParams);
    return c.json(service().create(params), 201);
  });

  routes.get('/:automation_id', (c) => {
    return c.json(service().get(c.req.param('automation_id')));
  });

  routes.post('/:automation_id', async (c) => {
    const automationId = c.req.param('automation_id');
    const params = await parsePlatformJson(c, UpdateAutomationParams);
    return c.json(service().update(automationId, params));
  });

  routes.delete('/:automation_id', (c) => {
    return c.json(service().remove(c.req.param('automation_id')));
  });

  routes.post('/:automation_id/pause', (c) => {
    return c.json(service().pause(c.req.param('automation_id')));
  });

  routes.post('/:automation_id/resume', (c) => {
    return c.json(service().resume(c.req.param('automation_id')));
  });

  routes.onError(automationErrorResponse);
  return routes;
}
