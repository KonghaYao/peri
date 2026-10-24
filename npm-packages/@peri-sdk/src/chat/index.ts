/**
 * Chat 控制面的模块内出口。
 *
 * 主入口何时公开这些符号由 Phase 4 的 `src/sdk/index.ts` 决定；这里不修改
 * package exports，也不把 Manager/Service 暴露给 `/portable`。
 */
export * from "./action";
export * from "./errors";
export * from "./facts";
export * from "./claims";
export * from "./binding";
export {
    ChatSendReceipt,
    InputCorrelation,
    type ChatSendReceiptOptions,
    type CorrelatedInput,
} from "./send-receipt";
export { ChatService } from "./service";
export { ChatManager } from "./manager";
export type { ChatManagerInternalOptions } from "./manager";
export type {
    ChatInstanceContext,
    ChatInstanceFactory,
    ChatSessionIndex,
    ChatServiceOptions,
} from "./service-support";
export { ChatFactHub } from "./fact-hub";
export type {
    CancelRequestResult,
    ChatBindingTarget,
    ChatDatabase,
    ChatDefaults,
    ChatHost,
    ChatManagerOptions,
    ChatSessionDirectoryEntry,
    ControlResult,
    HistoryPageOutcome,
    PeriInstancePort,
    SendReceipt,
    SessionClaims,
    SessionReadyResult,
} from "./types";
