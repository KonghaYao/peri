import { z } from 'zod';
import { CHAT_RUNTIME_REASONS, ChatRuntimeReason, SANDBOX_REASONS } from './chat';

export const API_ERROR_CODES = [
  'invalid_input',
  'invalid_action',
  'invalid_identity',
  'unsupported_mcp_type',
  'not_found',
  'binding_conflict',
  'request_conflict',
  'not_ready',
  'closed',
  'close_incomplete',
  'agent_has_chats',
  'revision_conflict',
  'provider_in_use',
  'credential_origin_confirmation_required',
  'session_creation_failed',
  'engine_unavailable',
  'internal_error',
] as const;

export const ApiErrorCode = z.enum(API_ERROR_CODES);
export type ApiErrorCode = z.infer<typeof ApiErrorCode>;

export const PUBLIC_ERROR_MESSAGES: Record<ApiErrorCode, string> = {
  invalid_input: 'Request validation failed',
  invalid_action: 'Chat action is not supported',
  invalid_identity: 'Workspace identity was rejected',
  unsupported_mcp_type: 'MCP transport is not supported',
  not_found: 'Requested resource not found',
  binding_conflict: 'Chat binding conflicts with the current configuration',
  request_conflict: 'requestId was already used with different text',
  not_ready:
    '运行配置未就绪；请选择共享 Provider 并在 Providers 管理页填写 SQLite API key，同时检查 Sandbox 状态',
  closed: 'Chat is closing or closed',
  close_incomplete: 'Chat and sandbox were not confirmed closed',
  agent_has_chats: 'Agent still has chats; delete the chats first',
  revision_conflict: '配置版本已变更或缺少 If-Match，请重新加载后再保存；未覆盖当前修改',
  provider_in_use: 'Provider 仍被 Agent 或未确认关闭的运行引用，不能删除',
  credential_origin_confirmation_required: '目标 origin 已改变，请明确确认现有凭据的新发送范围',
  session_creation_failed: 'Chat session creation failed',
  engine_unavailable: 'Chat runtime is unavailable',
  internal_error: 'Internal server error',
};

const DIAGNOSTIC_CODES = new Set<string>([
  ...API_ERROR_CODES,
  ...CHAT_RUNTIME_REASONS,
  ...SANDBOX_REASONS,
  'chat_close_failed',
  'sandbox_failed',
  'sandbox_start_failed',
  'sandbox_release_failed',
  'sandbox_close_incomplete',
  'sandbox_engine_missing',
  'sandbox_release_incomplete',
  'provider_missing',
  'api_key_missing',
]);

export function publicDiagnosticCode(value: unknown, fallback = 'internal_error'): string {
  return typeof value === 'string' && DIAGNOSTIC_CODES.has(value) ? value : fallback;
}

export type ApiErrorStatus = 400 | 404 | 409 | 500 | 502 | 503;

const STATUS_BY_CODE: Record<ApiErrorCode, ApiErrorStatus> = {
  invalid_input: 400,
  invalid_action: 400,
  invalid_identity: 400,
  unsupported_mcp_type: 400,
  not_found: 404,
  binding_conflict: 409,
  request_conflict: 409,
  not_ready: 409,
  closed: 409,
  close_incomplete: 409,
  agent_has_chats: 409,
  revision_conflict: 409,
  provider_in_use: 409,
  credential_origin_confirmation_required: 409,
  session_creation_failed: 502,
  engine_unavailable: 503,
  internal_error: 500,
};

export function httpStatusForErrorCode(code: ApiErrorCode): ApiErrorStatus {
  return STATUS_BY_CODE[code];
}

export const ApiErrorBody = z.strictObject({
  error: z.strictObject({
    code: ApiErrorCode,
    message: z.string().min(1),
    details: z.record(z.string(), z.unknown()),
    retryable: z.boolean(),
  }),
});
export type ApiErrorBody = z.infer<typeof ApiErrorBody>;
export type ApiErrorDetails = ApiErrorBody['error']['details'];

export function createApiErrorBody(
  code: ApiErrorCode,
  message: string,
  details: ApiErrorDetails = {},
  retryable = false,
): ApiErrorBody {
  return { error: { code, message, details, retryable } };
}

export const ENGINE_MODES = ['not_configured', 'embedded', 'remote'] as const;
export const ENGINE_STATUSES = ['unknown', 'starting', 'ready', 'unavailable'] as const;

export const EngineHealth = z.strictObject({
  mode: z.enum(ENGINE_MODES),
  status: z.enum(ENGINE_STATUSES),
  /** Why the chat runtime is not ready; codes only, never credentials or URLs. */
  reasons: z.array(ChatRuntimeReason).optional(),
});
export type EngineHealth = z.infer<typeof EngineHealth>;

export const HealthResponse = z.strictObject({
  ok: z.boolean(),
  engine: EngineHealth,
  runtime: z.strictObject({
    name: z.string(),
    version: z.string(),
  }),
});
export type HealthResponse = z.infer<typeof HealthResponse>;
