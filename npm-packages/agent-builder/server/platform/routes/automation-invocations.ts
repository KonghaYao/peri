import { Hono } from 'hono';
import { automationErrorResponse } from '../automations/errors';
import { automationRuntimeFor } from '../automations/runtime';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';

/** Routes for `/v1/agents/automations/{automation_id}/invocations`. */
export function createAutomationInvocationRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const service = () => automationRuntimeFor(repos).service;

  routes.get('/', (c) => {
    const page = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    return c.json(service().listInvocations(c.req.param('automation_id') ?? '', page));
  });

  routes.post('/', (c) => {
    // The documented contract answers the queued resource; delivery starts after the response.
    return c.json(service().manualInvoke(c.req.param('automation_id') ?? ''));
  });

  routes.get('/:invocation_id', (c) => {
    return c.json(
      service().getInvocation(c.req.param('automation_id') ?? '', c.req.param('invocation_id') ?? ''),
    );
  });

  routes.onError(automationErrorResponse);
  return routes;
}
