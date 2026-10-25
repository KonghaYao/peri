/**
 * Chat 控制面的模块内出口。
 *
 * 公开面由 `chat/public.ts` 定义，主入口与 `/portable` 共用（含
 * `ChatManager`/`ChatService`）；本文件只汇总模块内符号，不自行增减导出。
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
