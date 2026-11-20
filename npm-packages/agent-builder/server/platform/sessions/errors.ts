import type { Context } from 'hono';
import { type PlatformErrorStatus, platformError, platformErrorFromUnknown } from '../errors';

/**
 * Session-specific HTTP failures. The generic `AppError` table cannot express statuses such as
 * `409` with a session-scoped message, so the session routes map this class themselves.
 */
export class SessionHttpError extends Error {
  readonly code: string;
  readonly status: PlatformErrorStatus;
  readonly param: string | null;

  constructor(code: string, message: string, status: PlatformErrorStatus, param: string | null = null) {
    super(message);
    this.name = 'SessionHttpError';
    this.code = code;
    this.status = status;
    this.param = param;
  }
}

export function sessionErrorResponse(error: unknown, c: Context): Response {
  if (error instanceof SessionHttpError) {
    return platformError(c, error.code, error.message, error.status, error.param);
  }
  return platformErrorFromUnknown(error, c);
}
