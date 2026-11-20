import { Hono } from 'hono';
import { notImplemented } from '../errors';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';
import { SessionHttpError, sessionErrorResponse } from '../sessions/errors';
import { CreateSessionEventsParams } from '../sessions/params';
import { sessionRuntimeFor } from '../sessions/runtime';
import { SessionService } from '../sessions/service';

const MAX_IDEMPOTENCY_KEY = 256;

/** Routes for `/v1/agents/sessions/{session_id}/events`. */
export function createSessionEventRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();
  const runtime = () => {
    const installed = sessionRuntimeFor(repos);
    return installed === null ? null : new SessionService(installed);
  };

  routes.get('/', async (c) => {
    const service = runtime();
    const sessionId = c.req.param('session_id') ?? '';
    if (service === null) return notImplemented(c, 'session event list', [sessionId]);
    const tap = await service.stream(sessionId, c.req.raw.signal);
    return tap.response(200);
  });

  routes.post('/', async (c) => {
    const service = runtime();
    const sessionId = c.req.param('session_id') ?? '';
    if (service === null) return notImplemented(c, 'session event create', [sessionId]);
    const key = c.req.header('Idempotency-Key') ?? null;
    if (key !== null && (key.length === 0 || key.length > MAX_IDEMPOTENCY_KEY)) {
      throw new SessionHttpError(
        'invalid_input',
        'Idempotency-Key must be between 1 and 256 characters',
        400,
        'Idempotency-Key',
      );
    }
    const params = await parsePlatformJson(c, CreateSessionEventsParams);
    await service.submitInputs(sessionId, params, key);
    return c.body(null, 202);
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
