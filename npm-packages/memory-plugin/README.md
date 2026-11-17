# @peri/memory-plugin

一个 JS 入口，两种职责：`mcp` 提供 Hindsight 长期记忆；`import` 独立导入消息和 compact。完整设计见 [DESIGN.md](DESIGN.md)。

```bash
npm ci
npm run check
npm test
npm run build

export HINDSIGHT_BASE_URL=https://your-existing-hindsight.example
export HINDSIGHT_BANK_ID=existing-agent-bank
export MEMORY_AGENT_ID=peri-agent-example
export MEMORY_SESSION_ID=session-example
# 有鉴权的后端还需 HINDSIGHT_API_KEY；不要提交凭据。

node dist/index.js mcp
node dist/index.js import --file examples/messages.json --dry-run
node dist/index.js import --file examples/messages.json
```

- stdio：官方 MCP SDK 2.3.1 的 `serveStdio`，只支持 2026-07-28。
- HTTP：`mcp-use@2.12.4`，`mcp --transport http --port 33221`；只监听 loopback，需单独设置至少 32 字符的 MEMORY_MCP_TOKEN。
- 工具：memory_recall / memory_reflect / memory_context_refresh / memory_status。
- 资源：memory://context、memory://status，标准 subscriptions/listen。
- 同一个 Agent 的各 Session 共享长期 bank、分别启动上下文实例。导入器 agentId/sessionId 必须与环境绑定一致。
- 同一主机使用相同 MEMORY_STATE_DIR，导入完成会使已运行 MCP 快照过期。跨主机写入需显式 refresh。
- MCP 中没有 retain；不接入 MemoryMiddleware、Langfuse、数据库迁移或 Hindsight 部署。
- mcp-use 原生只提供 HTTP；stdio 使用公开 SDK 适配，未依赖框架私有接口。包内关闭遥测。

`npm pack` 产物可供 npx/bunx 启动。`./server`、`./stdio` 导出与根入口共用 `dist/index.js`；API 导出 createMemoryServer / startHttp / startStdio / importRecords 等函数。

测试使用本地 Hindsight HTTP 契约替身；真实后端连通性和记忆质量需要在部署环境验收。
