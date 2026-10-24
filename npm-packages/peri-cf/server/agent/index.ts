/** 服务端 Agent 链：唯一入口、会话服务与实例实现。 */
export { ChatManager, type ChatManagerOptions, type ChatManagerPort } from "./manager";
export {
  ChatService, mergeChatSummaries, type ChatServiceDeps, type ChatSubscription,
} from "./service";
