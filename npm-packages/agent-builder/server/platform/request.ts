import type { Context, MiddlewareHandler } from 'hono';
import type { z } from 'zod';
import { PlatformRequestError, platformError } from './errors';

const BEARER_PATTERN = /^Bearer +(\S+)$/i;

/**
 * The compatibility layer accepts any bearer token: authentication and project scoping
 * belong to the deployment, not to this process. Tokens are never logged or persisted.
 */
export function requirePlatformAuthorization(): MiddlewareHandler {
  return async (c, next) => {
    const header = c.req.header('authorization');
    if (header === undefined || !BEARER_PATTERN.test(header)) {
      return platformError(
        c,
        'authentication_required',
        'An Authorization header with a bearer token is required',
        401,
      );
    }
    return await next();
  };
}

export async function readPlatformJson(c: Context): Promise<unknown> {
  const contentType = c.req.header('content-type')?.toLowerCase() ?? '';
  if (!contentType.includes('application/json')) {
    throw new PlatformRequestError('Content-Type must be application/json');
  }
  try {
    return await c.req.json();
  } catch (error) {
    throw new PlatformRequestError('Request body must be valid JSON', null, error);
  }
}

/** Reads a JSON object body; arrays and primitives are rejected. */
export async function readPlatformJsonObject(c: Context): Promise<Record<string, unknown>> {
  const body = await readPlatformJson(c);
  if (typeof body !== 'object' || body === null || Array.isArray(body)) {
    throw new PlatformRequestError('Request body must be a JSON object');
  }
  return body as Record<string, unknown>;
}

/**
 * Parses a JSON body with a zod schema. Callers use strict objects so unknown fields
 * are rejected instead of silently ignored.
 */
export async function parsePlatformJson<T>(c: Context, schema: z.ZodType<T>): Promise<T> {
  const result = schema.safeParse(await readPlatformJson(c));
  if (!result.success) {
    const issue = result.error.issues[0];
    const path = issue === undefined ? '' : issue.path.map((segment) => String(segment)).join('.');
    throw new PlatformRequestError('Request validation failed', path.length > 0 ? path : null, result.error);
  }
  return result.data;
}
