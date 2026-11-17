import { z } from 'zod';

/** Mirrors the SDK's authoritative limit so the browser can validate before sending. */
export const SEND_TEXT_MAX_LENGTH = 32_768;

/**
 * Sandbox reasons are deployment codes, never paths, tokens or raw errors. The UI owns the
 * wording so environment variable names stay only where they belong.
 */
export const SANDBOX_REASONS = [
  'sandbox_peri_missing',
  'sandbox_settings_missing',
  'sandbox_settings_not_private',
  'sandbox_workspace_not_private',
  'sandbox_workspace_non_canonical',
  'sandbox_root_not_private',
  'sandbox_root_overlaps_workspace',
  'sandbox_token_missing',
  'sandbox_mcp_disabled',
] as const;
export const SandboxReason = z.enum(SANDBOX_REASONS);
export type SandboxReason = z.infer<typeof SandboxReason>;

export const SandboxStatus = z.strictObject({
  mode: z.enum(['disabled', 'ready', 'misconfigured']),
  reasons: z.array(SandboxReason),
});
export type SandboxStatus = z.infer<typeof SandboxStatus>;

export const ChatSandboxState = z.strictObject({
  state: z.enum(['disabled', 'starting', 'ready', 'releasing', 'closed', 'incomplete', 'error']),
  /** Stable code such as `sandbox_start_failed`; never a token, URL or raw error text. */
  reason: z.string().min(1).max(120).nullable(),
});
export type ChatSandboxState = z.infer<typeof ChatSandboxState>;

export const ChatSummary = z.strictObject({
  chatId: z.string().min(1).max(200),
  agentId: z.uuid(),
  agentName: z.string().min(1).max(80),
  sessionId: z.string().min(1).nullable(),
  createdAt: z.number().int().nonnegative(),
  updatedAt: z.number().int().nonnegative(),
  sandbox: ChatSandboxState.default({ state: 'disabled', reason: null }),
});
export type ChatSummary = z.infer<typeof ChatSummary>;

export const ChatListResponse = z.strictObject({ chats: z.array(ChatSummary) });
export type ChatListResponse = z.infer<typeof ChatListResponse>;

export const ChatResponse = z.strictObject({ chat: ChatSummary });
export type ChatResponse = z.infer<typeof ChatResponse>;

/** 可选关联标记仅接收，不持久化、不回显或记录任意文本。 */
export const ChatCreateRequest = z.strictObject({
  agentId: z.uuid(),
  traceId: z.string().trim().min(1).max(200).optional(),
});
export type ChatCreateRequest = z.infer<typeof ChatCreateRequest>;

export const ChatSendRequest = z.strictObject({
  requestId: z.string().min(1).max(200),
  text: z
    .string()
    .max(SEND_TEXT_MAX_LENGTH)
    .refine((value) => value.trim().length > 0, 'Message text cannot be blank'),
});
export type ChatSendRequest = z.infer<typeof ChatSendRequest>;

export const ChatSendResponse = z.strictObject({
  requestId: z.string().min(1),
  inputId: z.string().min(1),
  sessionId: z.string().min(1),
  bindingRevision: z.number().int().nonnegative(),
});
export type ChatSendResponse = z.infer<typeof ChatSendResponse>;

export const ChatCancelResponse = z.strictObject({
  requestId: z.string().min(1),
  status: z.enum(['requested', 'idle']),
});
export type ChatCancelResponse = z.infer<typeof ChatCancelResponse>;

export const ChatCloseResponse = z.strictObject({
  status: z.enum(['closed', 'close_incomplete']),
  errors: z.array(z.string()),
});
export type ChatCloseResponse = z.infer<typeof ChatCloseResponse>;

/**
 * Why the real runtime cannot start. Codes are stable; the UI owns the wording so environment
 * variable names are only mentioned where they belong.
 */
export const CHAT_RUNTIME_REASONS = [
  'store_token_missing',
  'store_unreachable',
  'store_auth_failed',
  'store_database_missing',
  'provider_key_missing',
  'wasm_assets_missing',
  'rivet_storage_path_missing',
] as const;
export const ChatRuntimeReason = z.enum(CHAT_RUNTIME_REASONS);
export type ChatRuntimeReason = z.infer<typeof ChatRuntimeReason>;

/**
 * Only a transient network/Store outage retries itself on the TTL. Credentials and database
 * configuration cannot change while the process runs, so those failures keep the controls disabled.
 */
export function isRetryableRuntime(reasons: readonly string[]): boolean {
  return reasons.length > 0 && reasons.every((reason) => reason === 'store_unreachable');
}

export const ChatRuntimeMode = z.enum(['wasm', 'not_configured']);
export type ChatRuntimeMode = z.infer<typeof ChatRuntimeMode>;

export const ChatRuntimeStatus = z.strictObject({
  mode: ChatRuntimeMode,
  reasons: z.array(ChatRuntimeReason),
});
export type ChatRuntimeStatus = z.infer<typeof ChatRuntimeStatus>;

export const ChatMessage = z.strictObject({
  id: z.string().min(1),
  role: z.enum(['user', 'assistant']),
  text: z.string(),
  status: z.string().min(1),
  createdAt: z.string().nullable(),
});
export type ChatMessage = z.infer<typeof ChatMessage>;

export const ChatStreamState = z.strictObject({
  running: z.boolean(),
  closed: z.boolean(),
});
export type ChatStreamState = z.infer<typeof ChatStreamState>;

export const CHAT_STREAM_EVENTS = ['snapshot', 'message', 'state', 'reset', 'closed'] as const;
export type ChatStreamEventName = (typeof CHAT_STREAM_EVENTS)[number];

export const ChatSnapshotEvent = z.strictObject({
  chatId: z.string().min(1),
  sessionId: z.string().min(1).nullable(),
  bindingRevision: z.number().int().nonnegative().nullable(),
  messages: z.array(ChatMessage),
  state: ChatStreamState,
  lastSeq: z.number().int().nonnegative(),
  overflow: z.boolean(),
});
export type ChatSnapshotEvent = z.infer<typeof ChatSnapshotEvent>;

export const ChatMessageEvent = z.strictObject({ message: ChatMessage });
export type ChatMessageEvent = z.infer<typeof ChatMessageEvent>;

export const ChatStateEvent = z.strictObject({ state: ChatStreamState });
export type ChatStateEvent = z.infer<typeof ChatStateEvent>;

export const ChatResetEvent = z.strictObject({
  reason: z.enum(['overflow', 'rebound']),
});
export type ChatResetEvent = z.infer<typeof ChatResetEvent>;

export const ChatClosedEvent = z.strictObject({
  reason: z.string().min(1).nullable(),
});
export type ChatClosedEvent = z.infer<typeof ChatClosedEvent>;
