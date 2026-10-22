/** 远程 Agent 客户端：HTTP 仓储、SSE 运行期与浏览器组合根。 */
export { createRemoteAgent, type RemoteAgentOptions } from "./application";
export { AgentManager, type AgentManagerDeps, type AgentView, type ChatLoadState, type ChatView, type Phase } from "./client";
export {
  clientCommandFailure, fromCommandError,
  type ClientCommandDisposition, type CommandFailure, type CommandOutcome,
} from "./commandFailure";
export { createHttpRepository } from "./repository";
export { RemoteChatRuntime } from "./runtime";
