/**
 * 服务端 Agent 组合根出口。
 *
 * 只暴露 CF 的平台装配：进程级 Manager、视图注册表与创建/关闭入口；
 * 句柄、动作与事实契约由 `@peri-code/sdk` 提供。
 */
export {
  AppChatManager,
  createAppChatManager,
  getChatManager,
  resetChatManagerForTest,
  shutdownChatManager,
  type AppChatManagerOptions,
  type ChatInstanceFactory,
} from "./manager";
export { ChatViewRegistry } from "./views";
