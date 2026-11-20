import type { Context } from 'hono';
import { PlatformRequestError, platformError, platformErrorFromUnknown } from '../errors';

export type EnvironmentErrorStatus = 400 | 404 | 409;

/** Reads a registered path parameter; Hono types it as possibly absent. */
export function requiredPathParam(c: Context, name: string): string {
  const value = c.req.param(name);
  if (value === undefined || value.length === 0) {
    throw new EnvironmentRequestError('invalid_input', 'Missing path parameter', 400);
  }
  return value;
}

/** Request failure raised by the environment family; carries the public wire code. */
export class EnvironmentRequestError extends Error {
  readonly code: string;
  readonly status: EnvironmentErrorStatus;
  readonly param: string | null;

  constructor(
    code: string,
    message: string,
    status: EnvironmentErrorStatus = 400,
    param: string | null = null,
  ) {
    super(message);
    this.name = 'EnvironmentRequestError';
    this.code = code;
    this.status = status;
    this.param = param;
  }
}

/**
 * Converts environment-family and platform request failures into the canonical
 * `{ error: { type, code, message, param } }` response. Anything else is left to the
 * platform error mapper so internal failures never echo request input.
 */
export function handleEnvironmentErrors(
  handler: (c: Context) => Response | Promise<Response>,
): (c: Context) => Promise<Response> {
  return async (c) => {
    try {
      return await handler(c);
    } catch (error) {
      if (error instanceof EnvironmentRequestError) {
        return platformError(c, error.code, error.message, error.status, error.param);
      }
      if (error instanceof PlatformRequestError) {
        return platformError(c, error.code, error.message, 400, error.param);
      }
      return platformErrorFromUnknown(error, c);
    }
  };
}
