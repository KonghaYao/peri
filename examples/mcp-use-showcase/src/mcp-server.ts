import { MCPServer } from "mcp-use";

/**
 * 唯一服务实例。
 *
 * mcp-use 2.x 的服务端是 stateless HTTP handler：`mcp-use dev` 负责监听，
 * 这里只做能力注册。所有状态放在模块作用域，回调本身必须可重复执行。
 */
export const server = new MCPServer({
  name: "mcp-use-showcase",
  version: "0.1.0",
  description:
    "mcp-use TypeScript SDK 能力展示：tools / resources / prompts / middleware / subscriptions / skills",
  instructions:
    "这是 mcp-use 能力展示服务器。开始验收时先调用 get_testing_guide；" +
    "技能 URI 为 skill://showcase-test-guide/SKILL.md。" +
    "需要读取单个笔记用 mcp_read_resource 读取 demo://notes/{id}。" +
    "改变状态后服务端会发送 resource updated 通知。",
  // 约定 skills/ 目录存在即自动发现，无需显式开启。
  // 兼容 Peri 等使用 2025-era initialize 的客户端；每个请求仍然独立处理。
  legacy: "stateless",
});
