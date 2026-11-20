import type { Context } from 'hono';
import { type PlatformErrorStatus, platformError, platformErrorFromUnknown } from '../errors';

/**
 * Automation-specific HTTP failures. The shared platform error table cannot express codes such as
 * `timezone_unsupported`, so automation routes map this class themselves.
 */
export class AutomationHttpError extends Error {
  readonly code: string;
  readonly status: PlatformErrorStatus;
  readonly param: string | null;

  constructor(code: string, message: string, status: PlatformErrorStatus, param: string | null = null) {
    super(message);
    this.name = 'AutomationHttpError';
    this.code = code;
    this.status = status;
    this.param = param;
  }
}

export function automationErrorResponse(error: unknown, c: Context): Response {
  if (error instanceof AutomationHttpError) {
    return platformError(c, error.code, error.message, error.status, error.param);
  }
  return platformErrorFromUnknown(error, c);
}
