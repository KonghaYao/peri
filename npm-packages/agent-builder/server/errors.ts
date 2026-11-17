import {
  type ApiErrorCode,
  type ApiErrorDetails,
  type ApiErrorStatus,
  createApiErrorBody,
  httpStatusForErrorCode,
  PUBLIC_ERROR_MESSAGES,
  publicDiagnosticCode,
} from '@shared/api';
import type { Context } from 'hono';
import { ZodError } from 'zod';

export interface AppErrorOptions {
  readonly details?: ApiErrorDetails;
  readonly retryable?: boolean;
  readonly cause?: unknown;
}

export class AppError extends Error {
  readonly code: ApiErrorCode;
  readonly details: ApiErrorDetails;
  readonly retryable: boolean;
  readonly status: ApiErrorStatus;

  constructor(code: ApiErrorCode, message: string, options: AppErrorOptions = {}) {
    super(message, { cause: options.cause });
    this.name = 'AppError';
    this.code = code;
    this.details = options.details ?? {};
    this.retryable = options.retryable ?? false;
    this.status = httpStatusForErrorCode(code);
  }

  toBody() {
    return createApiErrorBody(
      this.code,
      PUBLIC_ERROR_MESSAGES[this.code],
      safeDetails(this.details),
      this.retryable,
    );
  }
}

// 仅输出枚举与布尔字段；输入键名、校验文本也可能携带凭据。
function safeDetails(details: ApiErrorDetails): ApiErrorDetails {
  const safe: ApiErrorDetails = {};
  for (const key of ['reason', 'reasons', 'sandboxErrors'] as const) {
    const value = details[key];
    if (typeof value === 'string') safe[key] = publicDiagnosticCode(value);
    else if (Array.isArray(value)) safe[key] = value.map((item) => publicDiagnosticCode(item));
  }
  if (typeof details.actorClosed === 'boolean') safe.actorClosed = details.actorClosed;
  if (
    typeof details.status === 'string' &&
    ['unknown', 'starting', 'ready', 'unavailable'].includes(details.status)
  )
    safe.status = details.status;
  return safe;
}

export function diagnosticCode(error: unknown): string {
  if (typeof error !== 'object' || error === null) return 'internal_error';
  return publicDiagnosticCode((error as { code?: unknown }).code);
}

/** 校验异常作为 cause 留在内部，公共边界不回显输入或动态字段名。 */
export function appErrorFromZod(error: ZodError): AppError {
  return new AppError('invalid_input', PUBLIC_ERROR_MESSAGES.invalid_input, { cause: error });
}

export async function readJsonBody(c: Context): Promise<unknown> {
  try {
    return await c.req.json();
  } catch (error) {
    throw new AppError('invalid_input', 'Request body must be valid JSON', { cause: error });
  }
}

export function errorResponse(error: unknown, c: Context): Response {
  if (error instanceof AppError) {
    if (error.status >= 500) {
      console.error('[agent-builder] request failed', { code: diagnosticCode(error) });
    }
    return c.json(error.toBody(), error.status);
  }

  if (error instanceof ZodError) {
    const appError = appErrorFromZod(error);
    return c.json(appError.toBody(), appError.status);
  }

  console.error('[agent-builder] unhandled error', { code: diagnosticCode(error) });
  return c.json(createApiErrorBody('internal_error', 'Internal server error'), 500);
}
