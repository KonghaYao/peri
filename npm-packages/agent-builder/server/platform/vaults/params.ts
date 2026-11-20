import type { Context } from 'hono';
import { PlatformRequestError } from '../errors';

/**
 * Reads a path parameter registered by a parent mount, which Hono's route-local typing does
 * not expose. A missing parameter is a request error, never an empty resource ID.
 */
export function pathParam(c: Context, name: string): string {
  const value = c.req.param(name);
  if (value === undefined || value.length === 0) {
    throw new PlatformRequestError('Missing path parameter');
  }
  return value;
}
