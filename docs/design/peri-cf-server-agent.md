# peri-cf 服务端 Agent 执行

状态：现行设计（2026-10-21 用户裁决）。取代 [peri-cf 浏览器宿主 Agent](peri-cf-browser-agent.md) 中的执行位置与配置边界；浏览器宿主时期的生命周期、重放与命令语义仍作为历史依据保留。

## 目标

Peri 的 Agent 执行从浏览器标签页搬到独立的 Bun/Node 长驻进程：

- 服务端持有 ACP WASM Host、`ChatRegistry`、`ChatRuntime`、投影与 Turso/libSQL 仓储；
- 浏览器只做展示、命令转发与交互回答；
- 模型、Store 与 MCP 凭据只存在于服务端环境变量；
- 工具面只保留远程 Streamable HTTP MCP，不再随 Agent 执行浏览器本地文件系统或模拟 Bash。

这不是目录搬迁：浏览器不再启动 WASM 宿主，刷新/关闭页面不再结束执行。

## 边界与数据流

```text
浏览器 React
  → frontend/remote（HTTP + SSE）
  → /api/chats、/api/chats/:id、/api/chats/:id/commands、/api/chats/:id/events
  → server/api（Hono）
  → ChatService
  → ChatRegistry → ChatRuntime
  → WasmAcpTransport → peri-wasm ACP Host → 模型 / Store / 远程 MCP
```

- **共享内核**：`core/agent` 与 `core/shared` 平台无关，前端与服务端共同引用。前端把 `ChatRepository`、`ChatRuntimePort` 换成 HTTP/SSE 实现；服务端注入 WASM 宿主、Turso 仓储与远程 MCP 环境。
- **进程内生命周期**：`ChatRegistry` 按 ACP Session ID 惰性创建运行期，空闲 LRU 上限 8；执行中的运行期不回收。客户端断开只关闭订阅，不终止执行。
- **投影传输**：SSE 每帧发送完整 `SyncState`，50 ms 合并；重连后服务端重发完整快照，因此客户端无需增量状态即可自愈。SSE 只承载读面，命令仍走 HTTP。
- **历史复原**：冷启动仍由 `ChatRuntime.restore` 执行「先订阅、再 `session/load`、重放喂投影、关闭宿主」。展示态只在服务端内存；Store 是唯一持久权威。
- **机器身份**：`PERI_MACHINE_ID` 必须是持久 UUID，由部署方提供；服务端不随机生成，避免每次重启换身份。

## 凭据与工具面

- 模型 Provider/ID/Base URL/API Key、Store URL/token、machineId 与 MCP 配置全部从环境变量读取；前端没有 localStorage 配置、首次运行门禁或 relay。
- `PERI_MCP_SERVERS` 是 JSON 数组，只接受 `http(s)`；`token` 简写为 `Authorization: Bearer`。`session/new|load` 每次都按当前配置声明 MCP，不再按会话冻结快照。
- 后端不启动本地 ACP MCP；`mcp/message` 反向请求没有接收方。文件、终端、Web、Cron 等进程内 builtin 在 WASM 部署仍不可用，只能由远程 MCP 提供。
- `ServerHarnessConfig` 保持 `Todo`、`HumanInTheLoop`、`SubAgent`、`BuiltInSubagents`、`Mcp`、`ToolSearch`、`Goal`，其余 middleware 关闭。

## 协议与失败语义

- 命令契约在 `core/shared/chat.ts` 用 Zod 统一校验：`prompt`、`cancel`、`interaction`、`payload`。
- 服务端拒绝返回 HTTP 409 与 `{ ok:false, code, message, source:"server" }`；格式错误返回 400。前端 `ChatConnection` 继续消费稳定失败码。
- 会话不存在返回 404；SSE 在开始流之前先验证会话可复原，避免把错误塞进已开始的流。
- 非 `/api/*` 路径由同一 Bun 进程提供 SPA 静态资源与 `index.html` 回退。

## 安全与部署

- 没有内置认证、授权、限流或多租户隔离；只适合单可信用户或受入口控制保护的部署。反向代理鉴权是必要条件。
- 服务进程退出会结束所有在途执行；历史仍在 Store，执行不恢复。
- `/api/info` 只返回去敏后的地址（去掉 userinfo、query 与 token），前端设置页只读展示。
- Bun 服务是唯一后端入口；Cloudflare Worker relay 与 `wrangler.jsonc` 已删除。

## 与其他文档的关系

- 会话对象与投影契约仍以 [peri-cf-chat-objects.md](peri-cf-chat-objects.md) 为准。
- 服务端宿主的工具面与 WASM 限制见包 README 与 [peri-wasm 代码索引](../code-index/peri-wasm.md)。
- 入口与目录职责见 [peri-cf 代码索引](../code-index/peri-cf.md)。
