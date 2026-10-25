/**
 * Chat 控制面的公开符号（主入口与 portable 共用）。
 *
 * 只包含 Chat 句柄、动作、事实/视图契约与错误；已删除的旧业务门面
 * 不再从任何发布入口导出。
 */

export { ChatManager } from "./manager";
export { ChatService } from "./service";
export { AtomicSessionClaims } from "./claims";
export { ChatUserAction, parseChatUserAction } from "./action";
export * from "./errors";

export type {
    AnswerAction,
    CancelAction,
    ChatActionType,
    ChatUserActionBase,
    ChangeSessionAction,
    ControlAction,
    CreateNewSessionAction,
    ForceSendAction,
    HistoryAction,
    LoadEarlierHistoryAction,
    SendAction,
    SessionAction,
    TakeBackAction,
} from "./action";
export type {
    CancelRequestResult,
    ChatBindingTarget,
    ChatDatabase,
    ChatDefaults,
    ChatHost,
    ChatLifecycleOptions,
    ChatManagerOptions,
    ChatSessionDirectoryEntry,
    ChatService as ChatServiceContract,
    ControlResult,
    HistoryPageOutcome,
    PeriInstancePort,
    SendReceipt,
    SessionClaims,
    SessionReadyResult,
} from "./types";
export type {
    ChatFact,
    ChatFactEnvelope,
    ChatFactObserver,
    ChatFactSource,
    InteractionFactRecord,
    InteractionQuestion,
} from "./facts";

export {
    PayloadReadError,
    readPayload,
    limitPayloadReads,
    VIEW_PAYLOAD_MAX_BYTES,
    VIEW_PAYLOAD_MAX_CONCURRENCY,
} from "../view/payload-reader";
export type {
    PayloadReadErrorCode,
    ReadPayloadOptions,
    ToolPayloadReadRef,
    ToolPayloadReader,
    ToolPayloadResponse,
} from "../view/payload-reader";
export { ToolPayloadCapacityError } from "../state/tool-payloads";
export {
    TOOL_PAYLOAD_INLINE_BYTES,
    TOOL_PAYLOAD_PREVIEW_CHARS,
    TOOL_PAYLOAD_MAX_BYTES,
    SESSION_INLINE_PAYLOAD_BUDGET_BYTES,
    SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES,
} from "../state/tool-payloads";
export type { ToolPayloadBudgets, ToolPayloadRef } from "../state/tool-payloads";
export type { PublicError, PublicErrorCode } from "../state/doc-model";

export { ChatView } from "../view/chat-view";
export { ChatReplica } from "../view/chat-replica";
export type { ChatViewOptions, ChatViewSync } from "../view/chat-view";
export type { ChatReplicaConnection, ChatReplicaStatus } from "../view/chat-replica";
export type { ChatYjsChannel } from "../view/channel";
export {
    readSessionView,
    selectEntry,
    selectPendingInteractions,
    selectPlanForTurn,
    selectToolCall,
    selectTurn,
} from "../view/session-view";
export type {
    EntryBlockView,
    EntryView,
    InputQueueView,
    InteractionFieldView,
    InteractionQuestionView,
    InteractionView,
    PlanEntryView,
    SessionInfoView,
    SessionView,
    SessionViewReadiness,
    TaskView,
    ToolView,
    TurnView,
} from "../view/session-view";

export { parseInteractionAnswer } from "../agent/interaction-responder";
export type { InteractionAnswer } from "../agent/interaction-responder";
