import { describe, expect, test } from 'bun:test';
import { Hono } from 'hono';
import { z } from 'zod';
import { AppError } from '../../../server/errors';
import {
  notImplemented,
  PlatformRequestError,
  platformError,
  platformErrorBody,
  platformErrorFromUnknown,
  platformErrorTypeForStatus,
} from '../../../server/platform/errors';

function errorApp(): Hono {
  const app = new Hono();
  app.onError((error, c) => platformErrorFromUnknown(error, c));

  app.get('/app-error', () => {
    throw new AppError('not_found', 'Agent not found');
  });
  app.get('/request-error', () => {
    throw new PlatformRequestError('limit must be an integer', 'limit');
  });
  app.get('/zod-error', () => {
    throw z.object({ name: z.string() }).parse({});
  });
  app.get('/not-implemented', (c) => notImplemented(c, 'agent'));
  app.get('/missing-param/:id', (c) => notImplemented(c, 'agent', [c.req.param('id')]));
  app.get('/direct', (c) => platformError(c, 'rate_limited', 'Too many requests', 429, 'limit'));

  return app;
}

describe('platformErrorTypeForStatus', () => {
  test.each([
    [400, 'invalid_request_error'],
    [401, 'authentication_error'],
    [403, 'permission_error'],
    [404, 'not_found_error'],
    [409, 'conflict_error'],
    [429, 'rate_limit_error'],
    [500, 'server_error'],
    [501, 'not_implemented_error'],
    [503, 'server_error'],
  ] as const)('maps %i to %s', (status, type) => {
    expect(platformErrorTypeForStatus(status)).toBe(type);
  });
});

describe('platformErrorBody', () => {
  test('uses the canonical four-field error shape', () => {
    expect(platformErrorBody('not_found', 'Missing', 404)).toEqual({
      error: { type: 'not_found_error', code: 'not_found', message: 'Missing', param: null },
    });
  });
});

describe('platformErrorFromUnknown', () => {
  test('maps AppError without leaking its message', async () => {
    const response = await errorApp().request('/app-error');

    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({
      error: {
        type: 'not_found_error',
        code: 'not_found',
        message: 'Requested resource not found',
        param: null,
      },
    });
  });

  test('preserves the offending parameter for request errors', async () => {
    const response = await errorApp().request('/request-error');

    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({
      error: {
        type: 'invalid_request_error',
        code: 'invalid_input',
        message: 'limit must be an integer',
        param: 'limit',
      },
    });
  });

  test('maps zod failures to invalid_request_error with the issue path', async () => {
    const response = await errorApp().request('/zod-error');

    expect(response.status).toBe(400);
    const body = (await response.json()) as { error: { type: string; code: string; param: string } };
    expect(body.error.type).toBe('invalid_request_error');
    expect(body.error.code).toBe('invalid_input');
    expect(body.error.param).toBe('name');
  });
});

describe('notImplemented', () => {
  test('returns a 501 canonical error', async () => {
    const response = await errorApp().request('/not-implemented');

    expect(response.status).toBe(501);
    expect(await response.json()).toEqual({
      error: {
        type: 'not_implemented_error',
        code: 'not_implemented',
        message: 'The agent endpoint is not implemented',
        param: null,
      },
    });
  });

  test('validates the supplied path parameters', async () => {
    const response = await errorApp().request('/missing-param/agent_1');

    expect(response.status).toBe(501);
    expect(await response.json()).toMatchObject({ error: { code: 'not_implemented' } });
  });
});

describe('platformError', () => {
  test('honours the caller supplied status and param', async () => {
    const response = await errorApp().request('/direct');

    expect(response.status).toBe(429);
    expect(await response.json()).toEqual({
      error: {
        type: 'rate_limit_error',
        code: 'rate_limited',
        message: 'Too many requests',
        param: 'limit',
      },
    });
  });
});
