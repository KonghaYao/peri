/**
 * Agent 管理模块的公共出口。平台无关：只依赖 SDK 与 shared DTO；
 * 浏览器/后端差异经 `types.ts` 中的端口注入。客户端门面由 `frontend/remote` 提供。
 */
export {
  ChatCommandError, ChatNotFoundError,
  SessionCreator, defaultDefer,
  type ChatCommand, type ChatCommandPayload, type ChatRuntimePort, type ChatRuntimeSubscription,
  type RuntimeSummary,
} from "./session";
export {
  ChatInteractionQueue, ChatProjection,
  MAX_ASK_USER_QUESTIONS, buildAskUserQuestionContent,
  normalizeAskUserQuestionDecision, parseAskUserQuestionElicitation,
  type AskUserQuestionContent, type AskUserQuestionDecision, type AskUserQuestionDraft,
  type AskUserQuestionDrafts, type AskUserQuestionElicitation, type AskUserQuestionParse,
} from "./view";
export { initializeTransport } from "./transport";
export { errorText, logError, publicError } from "./logging";
export type {
  AgentEnvironment, ChatRepository, ChatSessionStore, Defer, McpAttachment, McpPurpose,
  McpSetupServer, StartHost, AcquireHost,
} from "./types";
export type { Chat, ChatSummary } from "./types";
export type { SyncState } from "../shared";
