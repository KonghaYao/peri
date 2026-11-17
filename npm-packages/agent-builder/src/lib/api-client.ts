import {
  ApiErrorBody,
  type ApiErrorCode,
  type ApiErrorDetails,
  HealthResponse,
  PUBLIC_ERROR_MESSAGES,
} from '@shared/api';
import type { z } from 'zod';

export class ApiClientError extends Error {
  readonly status: number;
  readonly code: ApiErrorCode | 'network_error';
  readonly details: ApiErrorDetails;
  readonly retryable: boolean;

  constructor(
    status: number,
    code: ApiErrorCode | 'network_error',
    message: string,
    details: ApiErrorDetails = {},
    retryable = false,
  ) {
    super(message);
    this.name = 'ApiClientError';
    this.status = status;
    this.code = code;
    this.details = details;
    this.retryable = retryable;
  }
}

export function describeApiError(error: unknown): string {
  if (error instanceof ApiClientError)
    return error.code === 'network_error' ? '无法连接到本地 API' : PUBLIC_ERROR_MESSAGES[error.code];
  return '请求失败，请重试';
}

async function perform(path: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(path, init);
  } catch {
    throw new ApiClientError(0, 'network_error', '无法连接到本地 API');
  }
}

async function toApiError(response: Response): Promise<ApiClientError> {
  const payload: unknown = await response.json().catch(() => null);
  const parsed = ApiErrorBody.safeParse(payload);
  if (parsed.success) {
    return new ApiClientError(
      response.status,
      parsed.data.error.code,
      PUBLIC_ERROR_MESSAGES[parsed.data.error.code],
      parsed.data.error.details,
      parsed.data.error.retryable,
    );
  }
  return new ApiClientError(
    response.status,
    'internal_error',
    `API 返回了未知错误（HTTP ${response.status}）`,
  );
}

/** Shared request path: one error envelope parser and one contract check for every endpoint. */
export async function apiRequest<T>(
  path: string,
  init: RequestInit | undefined,
  schema: z.ZodType<T>,
): Promise<T> {
  const response = await perform(path, init);
  if (!response.ok) throw await toApiError(response);

  const payload: unknown = await response.json().catch(() => null);
  const parsed = schema.safeParse(payload);
  if (!parsed.success) {
    throw new ApiClientError(response.status, 'internal_error', 'API 响应不符合契约');
  }
  return parsed.data;
}

export async function apiRequestVoid(path: string, init?: RequestInit): Promise<void> {
  const response = await perform(path, init);
  if (!response.ok) throw await toApiError(response);
}

export function jsonInit(method: 'POST' | 'PUT', body: unknown): RequestInit {
  return {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

export async function fetchHealth(): Promise<HealthResponse> {
  return apiRequest('/api/health', undefined, HealthResponse);
}
