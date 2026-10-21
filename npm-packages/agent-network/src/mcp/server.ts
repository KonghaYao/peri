import { McpServer } from '@modelcontextprotocol/server';

import type { Env } from '../env';
import type { Principal } from '../domain/principal';
import { registerResources } from './resources';
import { registerSkill } from './skill';
import { registerTools } from './tools';

const INSTRUCTIONS = [
  'Agent Network：通过标准 MCP 工具收发消息并领取 Agent 工作。',
  '开始或恢复工作时：',
  '1. network_register 绑定当前会话；已注册会返回同一身份。',
  '2. 订阅 agentnet://me 与自己 actor 的 inbox 资源，并先读一次当前状态。',
  '3. 收到 inbox 变更后调用 network_inbox_list 查看 pending 和 claimed。',
  '4. 没有状态不明的旧工作时，调用 network_delivery_claim 原子领取一条。',
  '5. 用 network_message_read 读取正文，必要时回读房间历史。',
  '6. 执行后调用 network_delivery_complete，可附带最终答复消息；失败用 network_delivery_fail。',
  '7. 仍有待办则继续下一条，否则保持订阅等待。',
  '普通回复不要填写 invoke；不要轮询；不要用长时间 tools/call 代替订阅。',
  '通知只表示资源发生变化，正文和业务状态始终以 D1 读取结果为准。',
].join('\n');

export function buildServer(env: Env, principal: Principal): McpServer {
  const server = new McpServer(
    { name: 'peri-agent-network', version: '0.1.0' },
    {
      capabilities: {
        tools: {},
        resources: { subscribe: true },
        // SEP-2640：声明 Skills 扩展后，客户端才走 skills/list 发现路径。
        extensions: { 'io.modelcontextprotocol/skills': {} },
      },
      instructions: INSTRUCTIONS,
    },
  );
  registerTools(server, env, principal);
  registerResources(server, env, principal);
  registerSkill(server);
  return server;
}
