# chat：微信风格的聊天 MCP demo（subscriptions/listen 全链路）

演示 MCP 2026-07-28 协议 `subscriptions/listen` 订阅机制：用户在聊天 UI 里
`@agent` 时，server 向已订阅的客户端推送 `notifications/resources/updated`，
peri（真实 MCP client）收到通知后唤醒 agent，agent 读取 `chat://` 资源并用
`chat/send` 工具回复。

## 运行

```bash
bun run chat/server.ts
```

- 聊天 UI：http://localhost:3100/ （微信风格：用户右侧绿泡、agent 左侧白泡）
- MCP 端点：http://localhost:3100/mcp （Streamable HTTP，协议 2026-07-28）
- 资源：`chat://room/general/messages`

## 在 peri 中接入

项目 `.mcp.json` 或 `~/.peri/settings.json` 增加：

```json
{
  "mcpServers": {
    "chat": {
      "url": "http://localhost:3100/mcp",
      "subscriptions": {
        "resources": ["chat://room/general/messages"]
      }
    }
  }
}
```

`subscriptions` 配置存在时，peri 连接会协商 2026-07-28 协议并建立
`subscriptions/listen` 长流（订阅字段说明见 `peri-acp-types/src/plugin.rs` 的
`McpSubscriptionsConfig`：`resources` URI 列表、`toolsListChanged`、
`promptsListChanged`、`resourcesListChanged`）。

### 通知 → 唤醒链路

1. 用户在聊天 UI 发送包含 `@agent` 的消息；
2. server 推送 `notifications/resources/updated`（`_meta` 带
   `io.modelcontextprotocol/subscriptionId`）到订阅流；
3. peri 的 `McpClientPool` 订阅循环（`spawn_subscription_loop`）收到通知，
   向所有已注册的会话 inbox 推送 Defer 消息（`<system-reminder><mcp-subscription …/>`）；
4. 会话被唤醒（TUI 路径 `await_wake`；stdio 路径队列积累到下一次 prompt），
   agent 读取资源 → `chat/send` 回复。

## 实现说明

- `server.ts` 为手写 JSON-RPC server（bun）：npm SDK 尚未发布 2026-07-28
  协议；`subscriptions/listen` 返回 SSE 流（`text/event-stream`），先发
  `notifications/subscriptions/acknowledged` 确认，再在资源变化时推送通知。
- `Bun.serve` 需 `idleTimeout: 0`：默认 10s 空闲超时会掐断订阅长流。
- server 状态在内存中（重启即清空），房间固定 `general`。
