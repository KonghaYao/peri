# peri-cf 服务端 Agent 执行

> 状态：**现行设计**。取代浏览器宿主时期的执行位置与配置边界；该时期设计已移入
> Git 历史。
> 对象契约见 [peri-cf 聊天对象](peri-cf-chat-objects.md)，Chat 控制面见
> [Peri SDK Chat](peri-sdk-chat.md)，入口以 [peri-cf 索引](../code-index/peri-cf.md) 为准。

## 目标

Peri 的 Agent 执行驻留在独立的 Bun/Node 长驻进程：

- 服务端持有 SDK `ChatManager`、`ChatService`、`PeriInstance`、`ChatView` 与 WASM ACP Host；
- 浏览器只做展示、Action 提交、交互回答与载荷读取，经 HTTP 提交，经 WebSocket 接收只读 Yjs 副本；
- 模型、Store 与远程 MCP 凭据只存在于服务端环境变量；
- 进程内 builtin（文件、终端、Web、Cron 等）在 WASM 部署不可用；工具面由远程 Streamable HTTP MCP 与每会话 sandbox MCP 组成，后者由 `peri-sandbox` 经 SDK `SandboxManager` 提供工作区、文件与终端能力。

刷新/关闭页面不结束执行；进程重启后可从 Store 重新加载历史并续聊，但不恢复旧 Agent 执行。

## 边界与数据流

```text
浏览器 React
  → frontend/remote（HTTP Action + WebSocket）
  → POST /api/chats、POST /api/chats/:chatId/actions、DELETE /api/chats/:chatId
  → GET /api/chats/:chatId/payload/:refId
  → WS /api/chats/:chatId/sync
  → server/index.ts（Hono 路由 + 静态资源 + Bun WebSocket）
  → ChatRegistry（ChatManagerContract + ChatViewRegistry + sandbox 租约）
  → SDK ChatManager → ChatService → PeriInstance → WasmAcpTransport → peri-wasm ACP Host
  → 模型 / Turso Store / 远程 MCP / peri-sandbox（工作区 + 文件、终端 MCP）
```

## 组合根与装配

- **平台装配**：`server/compose.ts` 读取 `ServerConfig`，构造 Store（`createChatDatabase`）、
  memory KV（`createStorage()`）、WASM `ChatRuntime`、共享 `session`（instructions + 远程 MCP；cwd 由
  Workspace 派生）与 `defaultWorkspace`（`id=default`，`/workspace`）；`chatManagerOptions(config, database)`
  是纯装配，入口用它构造具体 `ChatManager`。
- **每会话 sandbox**：`server/sandbox.ts` 用 SDK `SandboxManager` + `peri-sandbox` adapter 组装
  `SandboxPool`，按 `sandboxId` 引用计数；工作区固定为 `<root>/<sandboxId>/workspace`，MCP server key
  为 `sandbox`。`PERI_SANDBOX_ENABLED` 默认启用；禁用后回退固定 `/workspace`，历史会话仍可打开。
  关闭 Chat 释放租约但保留工作区，停机时 `closeAll`。
- **服务端注册表**：`server/registry.ts` 的 `ChatRegistry` 持有 `ChatManagerContract`、
  `ChatViewRegistry` 与每 Chat 的 sandbox 租约。`chats` 直接暴露句柄端口；`createChat` 按目标启动/复用
  sandbox 并把其 MCP 合并进 Session；`closeChat` 先关 SDK 句柄，再释放 View 与租约；`hasPendingClose`
  暴露「句柄已移除、View 或租约仍待释放」的残留；`shutdown` 依次 `disposeAll`、`chats.shutdown()`、
  `sandbox.closeAll()`。注册表只做关闭/停机编排与 `path → sessionPath`、`sessionId → sandbox` 解析。
- **入口**：`startServer(config)` 构造 database、sandbox pool 与 `ChatRegistry` 并组装 HTTP/WS；测试直接
  构造 `ChatRegistry` 或断言 `chatManagerOptions(config, database)`，生产路径没有可选注入参数。sandbox
  池经 `ChatRegistryOptions.sandbox` 端口注入，测试可用假实现替换。
- **应用只装配平台**：句柄、绑定、动作、事实与关闭语义都在 SDK；应用不复制准入、同步或关闭规则。
- **视图**：`ChatViewRegistry` 由 SDK 主入口与 `/portable` 提供（`/view` 不含），每个 Chat 一个
  `ChatView`；同一 Chat 的所有 WS 连接共享视图，连接断开不释放视图；绑定换代由 View 成对替换文档。
- **目录**：`GET /api/chats` 只读 Store，合并旧 `/workspace` 与各 sandbox 工作区的会话，不启动历史运行
  实例或 sandbox；列表不提供每会话运行态，运行/交互事实只来自选中会话的 Yjs 副本。
- **创建句柄**：`POST /api/chats` 的 body 可带 `sessionId` 或 `path`（互斥）。`sessionId` 用于解析该会话的
  sandbox 并在启动/复用后登记句柄；`path` 是旧接口的绝对会话 cwd，`ChatRegistry` 用 `node:path.relative`
  转为相对 `workspace.runtimePath` 的 `sessionPath`，空值表示工作区根，工作区外的绝对路径以
  `workspace_mismatch` 拒绝。无 body 时若启用 sandbox 则新建/取用一个 sandbox。SDK 由
  `workspace.runtimePath + sessionPath` 派生 `agent.cwd` 与新会话 cwd，`chatConfig.session` 不携带 `path`。
- **打开历史**：先 `POST /api/chats` 启动/复用 sandbox 并登记句柄，再 `change_session`；202 回执给出真实
  Session ID 与绑定代次后才连 WS。已有控制者返回 `binding_conflict`，前端改用 Session ID 寻址既有句柄。
- **新会话**：`create_new_session` 使用 `workspace` 派生的会话 cwd（省略 `sessionPath` 时为工作区根）；
  历史加载使用 Store 中的持久 cwd，sandbox 会话据此解析回 `<root>/<sandboxId>/workspace`，不回退句柄 path。
- **OpenUI 指令**：默认 Session instructions 由 `BASE_SYSTEM_INSTRUCTIONS` + OpenUI 段组成；OpenUI 段来自
  `@peri-code/sdk` 提交入库的 `src/openui/spec.json`，经 `@peri-code/sdk/openui/prompt` 的
  `generateOpenUISystemPrompt`（内部调用 `@openuidev/lang-core`）生成，服务端不 import React/canvas。
  提示词只随 `session/new` 冻结，**旧会话不会获得**；前端只在顶层完整
  `openui` 围栏，或流结束后以 `root = Stack(` 开头的裸程序时懒加载 Renderer；校验失败、引用未解析、
  `incomplete`、懒加载或渲染异常都回退 Markdown。表单值 ephemeral，`Button` action 经
  `parseOpenUIAction` 转成 `chat.send` 或 http(s) 外链（拒绝 userinfo、长度 >2048）。
- **同步**：WS 二进制帧为 HELLO/SNAPSHOT/UPDATE/ACK；客户端只读，不能写 Y.Doc 或提交业务命令。
  动作回执与投影是两条通道，不能用同步帧代替 Action 回执。
- **机器身份**：`PERI_MACHINE_ID` 必须是持久 UUID，由部署方提供；服务端不随机生成。

## 生命周期与失败语义

- `createChat` 同步登记并启动/复用 sandbox，但不启动 WASM、不取得 SessionOwnership；首次绑定动作才激活
  运行后端与 Session。`workspace` 与 `agent` 必填，`cwd` 由 SDK 从 `workspace.runtimePath` 注入，
  per-chat 显式值只能与其相同。
- 同一 Session 同时至多一个执行所有者。加载路径（`change_session`）冲突返回 `binding_conflict` 409（跨 Manager 不带
  `ownerChatId`），KV 读/写失败返回 `claim_failed` 500；新会话路径（`create_new_session`）claim 阶段失败
  （本地/KV 冲突或 KV 读/写失败）返回 `session_creation_failed` 500（stage `claim`，带 `createdSessionId`）。
  加载路径不启动 transport；新会话 claim 失败会启动并关闭临时 transport；两条路径都不提交绑定。没有通用 503 ownership 错误码。
- CF 固定 `createStorage()`（unstorage memory driver），只覆盖单进程；sandbox 引用计数也只在进程内。
  多实例需要改造装配接入共享 storage，并由路由层保证单实例绑定、换绑前 drain/清记录与崩溃清理。acquire
  是读后写、非原子，KV 记录是绑定标记/防御路由 bug，不是互斥原语。
- 关闭复用正常关闭与启动失败清理，并发关闭合并；确认清理后才移除索引与占用。`session/close` 超时
  返回 `ChatCloseIncompleteError`，保留 ownership、资源、sandbox 租约与原因，可重试。
- `shutdown` 分三段：`ChatRegistry` 先释放全部 View（断开同步连接与文档），再让 SDK 封闭新准入、结算在途
  关闭并释放 WASM 运行后端，最后 `sandbox.closeAll()`；任一段失败都聚合并保留资源，可重试。
- `cancel` 的 `requested` 不是停止证明；应用不能把 202 回执映射成 stopped。
- 动作错误以 `disposition` 表达可重试性：`rejected` 明确未生效，`unknown` 可能已执行且不得自动重试，
  `not_sent` 只由客户端在提交前产生。HTTP 状态不单独决定重试。
- 进程退出会结束在途执行；历史仍在 Store，执行不恢复。

## 凭据与工具面

- 模型 Provider/ID/Base URL/API Key、Store URL/token、machineId、远程 MCP 与 sandbox 配置全部从环境变量
  读取；前端没有 localStorage 配置、首次运行门禁或 relay。
- `PERI_MCP_SERVERS` 是 JSON 数组，只接受 `http(s)`；`token` 简写为 `Authorization: Bearer`。
  新会话与历史加载都按当前配置声明 MCP；`acp` 实例/工厂由使用方注入，SDK 不启动外部 Workspace。
- 服务端不启动 stdio/本地 ACP MCP；文件与终端能力由每会话 `peri-sandbox` 的 MCP server 提供，
  Web、Cron 等进程内 builtin 在 WASM 部署不可用。`PERI_MCP_SERVERS` 不得占用 sandbox 的 server key。
- `SERVER_HARNESS_CONFIG` 保持 Todo、HumanInTheLoop、SubAgent、BuiltInSubagents、Mcp、ToolSearch、
  Goal，其余 middleware 关闭。
- 模型与 Store 凭据只在服务端进程；前端构建不得出现模型/Store 凭据变量、Node 文件系统或 DB 驱动。

## 部署与安全

- 没有内置认证、授权、限流或多租户隔离；只适合单可信用户或受入口控制保护的部署。反向代理鉴权是
  必要条件。
- Bun 服务是唯一后端入口；Cloudflare Worker relay 与 `wrangler.jsonc` 已删除。
- `/api/monitor` 只读四段事实（LLM / MCP / WASM / Sandbox）：去敏地址、MCP header 名称、运行中 sandbox
  与进程级内存事实；读取不启动 sandbox 或 WASM。
- 单进程 ownership 与 sandbox 引用计数不能宣称跨实例接管；多实例部署需要改造装配接入共享 storage，
  并由路由层做串行化/崩溃清理。
- 真实模型/Store/远程 MCP、浏览器验收与 hosted 部署仍需环境验收；单元测试或类型检查不替代真实运行后端。
- OpenUI canvas 位于 `@peri-code/sdk/src/openui/canvas`，随 SDK 源码子路径发布；CF 以 `file:../@peri-sdk` 消费。

## 与其他文档的关系

- 对象契约与展示语义见 [peri-cf-chat-objects.md](peri-cf-chat-objects.md)。
- 前端渲染与 vendored 卡片边界见 [peri-cf-chat-ui.md](peri-cf-chat-ui.md)。
- SDK 的五个核心对象、事实/同步与关闭预算见 [peri-sdk-chat.md](peri-sdk-chat.md)。
- 入口与目录职责见 [peri-cf 代码索引](../code-index/peri-cf.md)。
