import type { Context } from 'hono';
import { Hono } from 'hono';
import { notImplemented } from '../errors';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';
import { SessionHttpError, sessionErrorResponse } from '../sessions/errors';
import { CreateSessionParams, UpdateSessionParams } from '../sessions/params';
import { sessionRuntimeFor } from '../sessions/runtime';
import { SessionService } from '../sessions/service';

const MAX_FILTER_LENGTH = 1_048_576;

function setDegradation(c: Context, reasons: readonly string[]): void {
  if (reasons.length > 0) c.header('x-platform-degraded', reasons.join(','));
}

/** Routes for `/v1/agents/sessions` and a single session resource. */
export function createSessionRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const runtime = () => {
    const installed = sessionRuntimeFor(repos);
    return installed === null ? null : new SessionService(installed);
  };

  routes.get('/', (c) => {
    const service = runtime();
    if (service === null) return notImplemented(c, 'session list');
    const page = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    const agentId = c.req.query('agent_id');
    if (agentId !== undefined && agentId.length > MAX_FILTER_LENGTH) {
      throw new SessionHttpError('invalid_input', 'agent_id is too long', 400, 'agent_id');
    }
    return c.json(
      service.list(page, {
        metadata: metadataQuery(c.req.url),
        ...(agentId === undefined ? {} : { agentId }),
      }),
    );
  });

  routes.post('/', async (c) => {
    const service = runtime();
    if (service === null) return notImplemented(c, 'session create');
    const params = await parsePlatformJson(c, CreateSessionParams);
    const created = await service.create(params, { signal: c.req.raw.signal });
    setDegradation(c, created.degradation);
    if (created.tap === null) return c.json(created.session, 201);
    return created.tap.response(201);
  });

  routes.get('/:session_id', (c) => {
    const service = runtime();
    const sessionId = c.req.param('session_id') ?? '';
    if (service === null) return notImplemented(c, 'session', [sessionId]);
    const row = service.require(sessionId);
    setDegradation(c, service.degradation(row));
    return c.json(service.resource(row));
  });

  routes.post('/:session_id', async (c) => {
    const service = runtime();
    const sessionId = c.req.param('session_id') ?? '';
    if (service === null) return notImplemented(c, 'session update', [sessionId]);
    const params = await parsePlatformJson(c, UpdateSessionParams);
    const updated = await service.update(sessionId, params);
    setDegradation(c, service.degradation(service.require(sessionId)));
    return c.json(updated);
  });

  routes.delete('/:session_id', async (c) => {
    const service = runtime();
    const sessionId = c.req.param('session_id') ?? '';
    if (service === null) return notImplemented(c, 'session delete', [sessionId]);
    return c.json(await service.remove(sessionId));
  });

  routes.onError(sessionErrorResponse);
  return routes;
}

/** Reads the documented `metadata[key]=value` deep-object query without accepting extra shapes. */
function metadataQuery(url: string): Record<string, string> {
  const metadata: Record<string, string> = {};
  const params = new URL(url).searchParams;
  for (const [rawKey, value] of params) {
    const match = /^metadata\[([^\]]+)\]$/.exec(rawKey);
    if (match === null) continue;
    const key = match[1] ?? '';
    if (key.length === 0 || key.length > 64 || value.length > 512) {
      throw new SessionHttpError('invalid_input', 'Invalid metadata filter', 400, 'metadata');
    }
    metadata[key] = value;
    if (Object.keys(metadata).length > 16) {
      throw new SessionHttpError('invalid_input', 'metadata accepts at most 16 pairs', 400, 'metadata');
    }
  }
  return metadata;
}
