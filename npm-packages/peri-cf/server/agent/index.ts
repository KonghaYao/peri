/**
 * 服务端 Agent 组合根出口。
 *
 * 只暴露 CF 的平台装配与编排入口；句柄、动作、事实与 View 契约由
 * `@peri-code/sdk` 提供。
 */
export {
  AppChatManager,
  getChatManager,
  shutdownChatManager,
  type AppChatManagerOptions,
  type ServerChatOptions,
} from "./manager";
export {
  createSdkChatManager,
  type ChatInstanceFactory,
  type LogError,
  type SdkChatManagerOptions,
} from "./compose";
