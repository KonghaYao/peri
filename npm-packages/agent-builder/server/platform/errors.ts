import { PUBLIC_ERROR_MESSAGES } from '@shared/api';
import type { Context } from 'hono';
import { ZodError } from 'zod';
import { AppError } from '../errors';

/** Error categories used by the platform wire format. */
export const PLATFORM_ERROR_TYPES = [
  'invalid_request_error',
  'authentication_error',
  'permission_error',
  'not_found_error',
  'conflict_error',
  'rate_limit_error',
  'not_implemented_error',
  'server_error',
] as const;

export type PlatformErrorType = (typeof PLATFORM_ERROR_TYPES)[number];

export type PlatformErrorStatus = 400 | 401 | 403 | 404 | 409 | 422 | 429 | 500 | 501 | 502 | 503;

export interface PlatformErrorBody {
  readonly error: {
    readonly type: PlatformErrorType;
    readonly code: string;
    readonly message: string;
    readonly param: string | null;
  };
}

const TYPE_BY_STATUS: Readonly<Record<PlatformErrorStatus, PlatformErrorType>> = {
  400: 'invalid_request_error',
  401: 'authentication_error',
  403: 'permission_error',
  404: 'not_found_error',
  409: 'conflict_error',
  422: 'invalid_request_error',
  429: 'rate_limit_error',
  500: 'server_error',
  501: 'not_implemented_error',
  502: 'server_error',
  503: 'server_error',
};

export function platformErrorTypeForStatus(status: PlatformErrorStatus): PlatformErrorType {
  return TYPE_BY_STATUS[status];
}

export function platformErrorBody(
  code: string,
  message: string,
  status: PlatformErrorStatus,
  param: string | null = null,
): PlatformErrorBody {
  return { error: { type: platformErrorTypeForStatus(status), code, message, param } };
}

/** Writes the canonical `{ error: { type, code, message, param } }` response. */
export function platformError(
  c: Context,
  code: string,
  message: string,
  status: PlatformErrorStatus,
  param: string | null = null,
): Response {
  return c.json(platformErrorBody(code, message, status, param), status);
}

/**
 * A documented capability this deployment cannot execute. The request is valid, but answering it
 * would require behavior the runtime does not implement, so it fails instead of being dropped.
 */
export class PlatformNotImplementedError extends Error {
  readonly code = 'not_implemented';
  readonly param: string | null;

  constructor(message: string, param: string | null = null) {
    super(message);
    this.name = 'PlatformNotImplementedError';
    this.param = param;
  }
}

/** Request failures raised by the platform layer that carry the offending parameter. */
export class PlatformRequestError extends AppError {
  readonly param: string | null;

  constructor(message: string, param: string | null = null, cause?: unknown) {
    super('invalid_input', message, cause === undefined ? {} : { cause });
    this.name = 'PlatformRequestError';
    this.param = param;
  }
}

function zodErrorParam(error: ZodError): string | null {
  const issue = error.issues[0];
  if (!issue) return null;
  const path = issue.path.map((segment) => String(segment)).join('.');
  return path.length > 0 ? path : null;
}

function appErrorStatus(error: AppError): PlatformErrorStatus {
  switch (error.status) {
    case 400:
    case 404:
    case 409:
    case 500:
    case 502:
    case 503:
      return error.status;
    default:
      return 500;
  }
}

/**
 * Maps internal errors to the platform wire format without echoing request input.
 * Messages come from the fixed public table, never from caught exception text.
 */
export function platformErrorFromUnknown(error: unknown, c: Context): Response {
  if (error instanceof PlatformNotImplementedError) {
    return platformError(c, error.code, error.message, 501, error.param);
  }
  if (error instanceof PlatformRequestError) {
    return platformError(c, error.code, error.message, appErrorStatus(error), error.param);
  }
  if (error instanceof AppError) {
    return platformError(c, error.code, PUBLIC_ERROR_MESSAGES[error.code], appErrorStatus(error));
  }
  if (error instanceof ZodError) {
    return platformError(c, 'invalid_input', 'Request validation failed', 400, zodErrorParam(error));
  }
  console.error('[agent-builder] platform request failed', { code: 'internal_error' });
  return platformError(c, 'internal_error', 'Internal server error', 500);
}

/**
 * Every platform route starts here until its handler is implemented.
 * Path values are passed in so the registration keeps parsing the documented parameters.
 */
export function notImplemented(
  c: Context,
  resource: string,
  params: readonly (string | undefined)[] = [],
): Response {
  if (params.some((value) => value === undefined || value.length === 0)) {
    return platformError(c, 'invalid_input', 'Missing path parameter', 400);
  }
  return platformError(c, 'not_implemented', `The ${resource} endpoint is not implemented`, 501);
}
