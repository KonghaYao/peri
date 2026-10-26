/** 远程 Chat 客户端：HTTP Action、WebSocket + Yjs 同步与页面控制器。 */
export { createChatApplication, type ChatApplicationOptions } from "./application";
export {
  RemoteActionError, RemoteSendReceipt, createRemoteActions,
  parseActionTarget, type ActionReceipt, type RemoteActions, type RemoteChatHandle, type RemoteDisposition,
} from "./actions";
export {
  ChatController,
  type ChatLoadState, type ChatView, type Phase, type SyncStatus,
} from "./controller";
export {
  ChatDirectoryUnavailableError, createHttpRepository,
  type ChatDirectoryRepository,
} from "./repository";
export {
  ChatSyncProvider, SyncBackpressureError, WebSocketChatChannel,
  type ChatSyncOptions, type SyncConnectionStatus, type WebSocketLike,
} from "./sync";
export * from "./contracts";
export * from "./display-state";
export * from "./projection";
export * from "./askUserQuestion";
export { logError } from "./logging";
