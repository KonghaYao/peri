# peri-cf 服务端 Agent 执行

> 状态：**现行设计**（2026-10-26）。取代 [peri-cf 浏览器宿主 Agent](peri-cf-browser-agent.md)
> 中的执行位置与配置边界；浏览器宿主时期的设计只作历史依据。
> 对象契约见 [peri-cf 聊天对象](peri-cf-chat-objects.md)，Chat 控制面见
> [Peri SDK Chat](peri-sdk-chat.md)，入口以 [peri-cf 索引](../code-index/peri-cf.md) 为准。

## 目标

Peri 的 Agent 执行驻留在独立的 Bun/Node 长驻进程：

- 服务端持有 SDK `ChatManager`、`ChatService`、`PeriInstance`、`ChatView` 与 WASM ACP Host；
- 浏览器只做展示、Action 提交与交互回答，经 HTTP 提交动作、经 WebSocket 接收只读 Yjs 副本；
- 模型、Store 与 MCP 凭据只存在于服务端环境变量；
- 工具面只保留远程 Streamable HTTP MCP，不随 Agent 执行浏览器本地文件系统或模拟 Bash。

刷新/关闭页面不结束执行；进程重启后可从 Store 重新加载历史并续聊，但不恢复旧 Agent 执行。

## 边界与数据流

```text
浏览器 React
  → frontend/remote（HTTP Action + WebSocket）
  → POST /api/chats、POST /api/chats/:id/actions、DELETE /api/chats/:id
  → GET /api/chats/:id/payload/:refId
  → WS /api/chats/:id/sync
  → server/api（Hono + Bun WebSocket）
  → AppChatManager（注入 ChatManagerContract + ChatViewRegistry）
  → SDK ChatManager → ChatService → PeriInstance → WasmAcpTransport → peri-wasm ACP Host
  → 模型 / Turso Store / 远程 MCP
```

## 组合根与装配

- **平台装配**：`server/agent/compose.ts` 读取 `ServerConfig`，构造 Store 适配、ownership 存储（默认 `createStorage()`）、
  WASM `ChatRuntime`、默认 path/instructions/MCP 与关闭预算；server 内只有这里构造具体 `ChatManager`。
- **编排层**：`server/agent/manager.ts` 的 `AppChatManager` 只持有注入的 `ChatManagerContract` 与
  `ChatViewRegistry`。四个查询/登记方法直接转发；`closeChat` 先关 SDK 句柄再释放 View；
  `hasPendingClose` 暴露「句柄已移除、View 仍待释放」的残留；`shutdown` 先 `disposeAll` 再
  `chats.shutdown()`。编排层不重写领域规则。
- **唯一注入点**：`getChatManager` 首次调用才装配进程单例；测试经 `ServerChatOptions` 注入端口，
  生产入口只在 `server/index.ts` 调用。审核 ports 与生命周期时以 `manager.ts` 为主，平台默认值集中在
  `compose.ts`。
- **应用只装配平台**：句柄、绑定、动作、事实与关闭语义都在 SDK；应用不复制准入、同步或关闭规则。
- **视图**：`ChatViewRegistry` 由 SDK 主入口与 `/portable` 提供（`/view` 不含），每个 Chat 一个
  `ChatView`；同一 Chat 的所有 WS 连接共享视图，连接断开不释放视图；绑定换代由 View 成对替换文档。
- **目录**：`GET /api/chats` 只读 Store，不启动历史运行实例；列表不提供每会话运行态，运行/交互事实
  只来自选中会话的 Yjs 副本。
- **打开历史**：先 `POST /api/chats` 登记空句柄，再 `change_session`；202 回执给出真实 Session ID 与
  绑定代次后才连 WS。已有控制者返回 `binding_conflict`，前端改用 Session ID 寻址既有句柄。
- **新会话**：`create_new_session` 使用默认 path；历史加载使用 Store 中的持久 cwd，不回退句柄 path。
- **同步**：WS 二进制帧为 HELLO/SNAPSHOT/UPDATE/ACK；客户端只读，不能写 Y.Doc 或提交业务命令。
  动作回执与投影是两条通道，不能用同步帧代替 Action 回执。
- **机器身份**：`PERI_MACHINE_ID` 必须是持久 UUID，由部署方提供；服务端不随机生成。

## 生命周期与失败语义

- `createChat` 同步登记，不启动 WASM、不取得 SessionOwnership；首次绑定动作才激活运行后端与 Session。
- 同一 Session 同时至多一个执行所有者。加载路径（`change_session`）冲突返回 `binding_conflict` 409（跨 Manager 不带
  `ownerChatId`），KV 读/写失败返回 `claim_failed` 500；新会话路径（`create_new_session`）claim 阶段失败
  （本地/KV 冲突或 KV 读/写失败）返回 `session_creation_failed` 500（stage `claim`，带 `createdSessionId`）。
  加载路径不启动 transport；新会话 claim 失败会启动并关闭临时 transport；两条路径都不提交绑定。没有通用 503 ownership 错误码。
- CF 默认 `createStorage()`（unstorage memory driver）只覆盖单进程；多实例需要共享 storage，并由路由层保证
  单实例绑定、换绑前 drain/清记录与崩溃清理。acquire 是读后写、非原子，KV 记录是绑定标记/防御路由 bug，
  不是互斥原语。
- 关闭复用正常关闭与启动失败清理，并发关闭合并；确认清理后才移除索引与占用。`session/close` 超时
  返回 `ChatCloseIncompleteError`，保留 ownership、资源与原因，可重试。
- `shutdown` 分两段：编排层先释放全部 View（断开同步连接与文档），再让 SDK 封闭新准入、结算在途
  关闭并释放 WASM 运行后端；两段失败都聚合并保留资源，可重试。
- `cancel` 的 `requested` 不是停止证明；应用不能把 202 回执映射成 stopped。
- 动作错误以 `disposition` 表达可重试性：`rejected` 明确未生效，`unknown` 可能已执行且不得自动重试，
  `not_sent` 只由客户端在提交前产生。HTTP 状态不单独决定重试。
- 进程退出会结束在途执行；历史仍在 Store，执行不恢复。

## 凭据与工具面

- 模型 Provider/ID/Base URL/API Key、Store URL/token、machineId 与 MCP 配置全部从环境变量读取；
  前端没有 localStorage 配置、首次运行门禁或 relay。
- `PERI_MCP_SERVERS` 是 JSON 数组，只接受 `http(s)`；`token` 简写为 `Authorization: Bearer`。
  新会话与历史加载都按当前配置声明 MCP；`acp` 实例/工厂由使用方注入，SDK 不启动外部 Workspace。
- 后端不启动本地 ACP MCP；文件、终端、Web、Cron 等进程内 builtin 在 WASM 部署不可用，只能由远程
  MCP 提供。
- `SERVER_HARNESS_CONFIG` 保持 Todo、HumanInTheLoop、SubAgent、BuiltInSubagents、Mcp、ToolSearch、
  Goal，其余 middleware 关闭。
- 模型与 Store 凭据只在服务端进程；前端构建不得出现模型/Store 凭据变量、Node 文件系统或 DB 驱动。

## 部署与安全

- 没有内置认证、授权、限流或多租户隔离；只适合单可信用户或受入口控制保护的部署。反向代理鉴权是
  必要条件。
- Bun 服务是唯一后端入口；Cloudflare Worker relay 与 `wrangler.jsonc` 已删除。
- `/api/info` 只返回去敏后的地址（去掉 userinfo、query 与 token），设置页只读展示。
- 单进程 ownership 不能宣称跨实例接管；多实例部署必须由部署方提供共享 storage 与路由层串行化/崩溃清理。
- 真实模型/Store/远程 MCP、浏览器验收与 hosted 部署仍需环境验收；单元测试或类型检查不替代真实运行后端。

## 与其他文档的关系

- 对象契约与展示语义见 [peri-cf-chat-objects.md](peri-cf-chat-objects.md)。
- 前端渲染与 vendored 卡片边界见 [peri-cf-chat-ui.md](peri-cf-chat-ui.md)。
- SDK 的五个核心对象、事实/同步与关闭预算见 [peri-sdk-chat.md](peri-sdk-chat.md)。
- 入口与目录职责见 [peri-cf 代码索引](../code-index/peri-cf.md)。
