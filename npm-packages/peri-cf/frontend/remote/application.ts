import { AgentManager } from "./client";
import { createHttpRepository } from "./repository";
import { RemoteChatRuntime } from "./runtime";

export interface RemoteAgentOptions {
  /** API 同源 base；测试可注入。 */
  origin?: string;
}

/**
 * 浏览器组合根：Agent 在服务端执行，前端只提供读面、命令与交互回答。
 * 没有本地 WASM 宿主、localStorage 配置或本地 MCP。
 */
export function createRemoteAgent(options: RemoteAgentOptions = {}): AgentManager {
  const origin = options.origin ?? globalThis.location.origin;
  // 最近一次会话的运行期复用：读面与订阅共用实例，一次选择不会各自建立一条 SSE 流。
  // 只保留一个，避免历史会话的完整投影长期驻留浏览器内存。
  let cached: { chatId: string; runtime: RemoteChatRuntime } | undefined;
  const runtimeFor = (chatId: string): RemoteChatRuntime => {
    if (cached?.chatId !== chatId) cached = { chatId, runtime: new RemoteChatRuntime(chatId, origin) };
    return cached.runtime;
  };
  return new AgentManager({
    repository: createHttpRepository(origin),
    runtimeFor,
  });
}
