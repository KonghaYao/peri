# Peri CF 代码索引

状态：现行实现。Bun 长驻服务是唯一计算实例，在进程内启动 peri-wasm ACP Host；浏览器只消费
会话目录、Action 回执与只读 Yjs 副本。凭据与远程 MCP 只存在于服务端环境变量。独立于根 Cargo
workspace；使用说明见 [包 README](../../npm-packages/peri-cf/README.md)，设计见
[服务端 Agent](../design/peri-cf-server-agent.md) 与 [Chat 前端](../design/peri-cf-chat-ui.md)。

## 边界与数据流

```text
浏览器 React
  → frontend/remote：HTTP Action + WebSocket 二进制同步
  → server/api：目录 / 句柄 / Action / payload / WS upgrade
  → @peri-code/sdk：ChatManager → ChatService → PeriInstance → ACP
  → peri-wasm Host（每次激活一个）→ 模型 / Turso Store / 远程 MCP
  → peri-sandbox（每会话一个）→ 本地工作区 / 文件与终端 MCP
```

- **平台装配**：`server/compose.ts` 把 WASM 运行后端、Store、memory KV 与默认配置装配成
  `chatManagerOptions(config)`，可直接断言；入口用它构造具体 `ChatManager`。
- **生产测试场**：依赖 registry 上的 `@peri-code/sdk` 固定版本；`server/tsconfig.json` 不映射 SDK
  源码，`scripts/wasm-tools.ts` 从 `node_modules` 校验已安装包的 `exports["./wasm"]`。
  `server/sandbox-adapter.test.ts` 校验本地 `peri-sandbox` adapter 与已发布 `/sandbox` 的契约。
- **每会话 sandbox**：`server/sandbox.ts` 用 SDK `SandboxManager` + `peri-sandbox` adapter 组装租约池，
  按 `sandboxId` 引用计数；`server/registry.ts` 在 `createChat` 前启动/复用实例，把
  `workspace` 与 MCP `sandbox` 合并进 Session；历史会话由 Store `cwd` 解析
  `<root>/<sandboxId>/workspace`，旧 `/workspace` 会话走固定工作区。关闭 Chat 释放租约并保留工作区，
  停机 `closeAll`；启动失败映射 503 `sandbox_start_failed`。sandbox MCP 是透明代理：能力、
  tools、resources、prompts、skills 与通知都从内层 `peri mcp-start workspace` 镜像/转发；server 名仍为
  `sandbox`，不作为宿主 `WorkspaceRemote` 系统资源面。
- **服务端注册表**：`server/registry.ts` 的 `ChatRegistry` 持有 `ChatManagerContract` 与
  `ChatViewRegistry`，`chats` 直接暴露句柄端口，编排 close 与 shutdown；`createChat` 不启动 WASM，
  不读历史，只按目标启动/复用 sandbox。
- **视图**：`ChatViewRegistry` 由 SDK 提供（主入口与 `/portable`，不在 `/view`），每个 Chat 一个
  `ChatView`；同一 Chat 的多个 WS 连接共享视图，断开连接不释放视图；绑定换代由 View 成对替换文档。
  `closeChat` 先关句柄再释放视图，`shutdown` 先 `disposeAll` 再关闭 SDK 端口。
- **HTTP**：`server/api/index.ts` 只做边界校验、目标绑定核对与错误映射；执行、绑定与关闭语义在 SDK。
- **WS + Yjs**：`server/index.ts` 用 `@hono/bun` 的 `upgradeWebSocket` + `websocket` 升级；
  `server/api/sync.ts` 的 `syncUpgradeGuard` 先校验方法/Upgrade 头并把逐连接状态挂到
  `SyncConnection`，`syncEvents` 再把每连接 `WSContext` 适配为 `ChatYjsChannel`；客户端只能发
  HELLO/ACK，任何文档更新帧都被拒绝；慢客户端由 SDK 交付信用/背压显式关闭。
- **前端**：`frontend/remote/controller.ts` 是页面控制器（句柄身份、两步打开历史、绑定代次、Action、
  连接）；视图派生纯函数在 `view-state.ts`，投递结算在 `delivery.ts`，句柄表在 `handles.ts`
  （三者不持有连接生命周期）；`actions.ts` 是 HTTP 客户端；`sync.ts` 是 WebSocket → SDK 副本；
  `application.ts` 是前端组合根。
  `frontend/remote/projection.ts` 把 `ChatReplica` 物化为展示 DTO，不反向控制执行。
- **生成式 UI**：组件库、spec 与 Renderer 作为 `@peri-code/sdk/openui*` 源码 subpath，见
  [`peri-openui`](peri-openui.md)；`npm-packages/@peri-sdk/src/openui/library.tsx` 是组件事实源，
  `bun run openui:spec` 生成 `npm-packages/@peri-sdk/src/openui/spec.json`。
  CF 的 `frontend/chat/openui/` 只保留 `OpenUIMessage` 聊天适配，它只认顶层完整 `openui` 围栏并懒加载
  `@peri-code/sdk/openui/renderer`；
  root 为空、root 存在但 prop 校验失败/引用未解析、incomplete、未闭合或渲染异常都回退 Markdown。
  流结束后的裸 `root = Stack(` 程序会被识别，普通代码块/列表项内的 `openui` 字样不算；服务端经
  `@peri-code/sdk/openui/prompt` 只读 `spec.json` 生成提示词，随 `session/new` 对新会话生效。
- **静态资源**：非 `/api/*` 路径由 `server/index.ts` 的 Hono `serveStatic` 提供 `dist/client` 与 `index.html` 回退；Vite 开发服务器把
  `/api` 代理到 Bun 服务。
- **没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。
- **单进程 ownership**：`server/compose.ts` 固定使用 `createStorage()`（unstorage memory driver）作为 `kv`，由 `ChatManager` 内部构造协调器；多实例部署需要改造装配接入共享 storage，并由路由层做串行化/崩溃清理，不能宣称
  跨实例接管。

## 路由

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/monitor` | 四段只读监控快照（LLM / MCP / WASM 实例 / Sandbox）；地址去敏，MCP header 只给名称；WASM 段含每实例线性内存、回合/token/工具计数与关闭未确认状态 |
| GET | `/api/chats` | 只读 Store 目录；不可达返回 503 + `available:false`，不伪造空列表 |
| POST | `/api/chats` | 按目标启动/复用 sandbox 并登记句柄，返回 201 + `{chat:{id,sessionId:null,bindingRevision:0}}`；不新建 Session；body 可带 `sessionId` 或 `path`（互斥），sandbox 启动失败 503 `sandbox_start_failed` |
| POST | `/api/chats/:chatId/actions` | `ChatUserAction` + 目标绑定；202 回执带 `inputId`/`status`/`applied` 等纯数据 |
| DELETE | `/api/chats/:chatId` | `closeChat`；成功 204；未确认关闭 500 + `incomplete`/`errors`；句柄已移除但 View 未释放时，同一 `chatId` 的 DELETE 会重试释放并返回 204 |
| GET | `/api/chats/:chatId/payload/:refId?bytes=` | 当前激活的载荷仓库；错误映射为公开 payload code |
| WS | `/api/chats/:chatId/sync` | 只读 Yjs 同步；未知 Chat 在升级前 410 |

## 两步打开历史

1. `POST /api/chats` 取得空句柄（`sessionId:null`，`bindingRevision:0`）；历史会话 body 带
   `sessionId`，服务端据此解析并启动/复用其 sandbox；新建会话不带 body。
2. `POST .../actions` 发送 `change_session`；202 回执给出真实 Session ID 与新 `bindingRevision`。
3. 只有绑定成功后，前端才为该句柄打开 `/sync` WebSocket 并等待首屏 SNAPSHOT。
4. 若服务端已有该 Session 的控制者，`change_session` 返回 `binding_conflict`；前端释放候选句柄，改用
   Session ID 寻址既有句柄，不重复绑定。切换页面只释放本地连接，不 cancel、不 close。
5. 新会话走 `create_new_session`，同样等待 202 后才连接；创建不写标题（标题由 Store 在首次
   `append_history` 时从首条用户消息补齐，前端只提供展示回退）；失败时清理候选句柄。

## 错误映射

- 加载路径已有控制者：409 `binding_conflict`。
- 加载路径 KV 读/写失败：500 `claim_failed`。
- 新会话 claim 阶段失败（本地/KV 冲突或 KV 读/写失败）：500 `session_creation_failed`（stage `claim`）。
- 没有通用 503 ownership 错误码；`closed` 仍是独立的 503。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| Hono 应用 + Bun 入口（HTTP + 静态资源 + WS + 信号） | `server/index.ts` |
| 环境变量解析与启动校验 | `server/config.ts` |
| 平台装配（`chatManagerOptions`） | `server/compose.ts` |
| 服务端注册表（句柄端口 + View 生命周期） | `server/registry.ts` |
| 每实例业务计数（事实流订阅，不保存内容） | `server/instance-metrics.ts` |
| HTTP 路由与错误映射 | `server/api/index.ts`、`server/api/errors.ts` |
| WebSocket 同步适配 | `server/api/sync.ts` |
| 共享 Zod DTO / HTTP 契约 | `frontend/remote/contracts.ts`、`frontend/remote/display-state.ts` |
| 浏览器投影 | `frontend/remote/projection.ts` |
| 远程客户端与页面控制器 | `frontend/remote/{application,actions,controller,view-state,delivery,handles,repository,sync}.ts` |
| Chat 运行时与 useChat | `frontend/chat/runtime/{useChat,queries,payloadReader,askUserQuestion}.ts` |
| Chat 展示与交互 | `frontend/chat/{components,timeline,composer}/`；`components/shell/` 承载草稿/导航 hook 与横幅，`composer/` 承载输入、状态栏、上下文用量、限长、历史召回、斜杠命令与草稿持久化；`timeline/toolText.ts` 统一折叠/失败态文本提取；`frontend/components/assistant-ui/` 只保留 vendored 审批/问答卡片 |
| OpenUI 聊天适配 | `frontend/chat/openui/{OpenUIMessage,actions,index}.ts(x)`；渲染与 spec 见 [`OpenUI 代码索引`](peri-openui.md) |
| OpenUI 系统提示词 | `server/openui-instructions.ts`、`server/system-instructions.ts` |
| SPA 外壳与只读服务端监控 | `frontend/app/App.tsx`、`frontend/settings/MonitorDialog.tsx`、`frontend/settings/monitor/` |
| Vite 构建与 `/api` 代理 | `vite.config.ts` |
| WASM 构建与 provenance | `scripts/{build-wasm,prepare-wasm,wasm-provenance,wasm-profile}.ts` |

## 构建前置

CF 的 `typecheck`/`test` 通过 `file:../@peri-sdk` 解析到 `node_modules/@peri-code/sdk`。干净 CI 先在
`npm-packages/@peri-sdk` 执行 `MISE_ENV=wasm mise exec -- bun run build`，再在 CF 执行
`bun install --force`；该条目会变成指向工作区包的符号链接，直接消费最新 `dist`。SDK build 依赖
Emscripten 6.0.10 与 peri-wasm 产物。最近一次记录（2026-10-26）：SDK typecheck 通过、`bun test`
407 pass / 0 fail（49 files、2947 expect）；CF typecheck 通过、`bun run test` 248 pass / 0 fail
（根测试 203 + server 45；30 + 6 files）；CF `vite build` 通过（2408 modules）；未运行 emcc build、
`test:wasm` 与 E2E，Hosted Workers 部署未验收。

## 验证入口

- `bun run typecheck`：根 TS + `server/tsconfig.json`。
- `bun test`：`frontend`、`scripts`，再以 server tsconfig 跑 `server`。
- `bun run openui:spec` / `openui:check`：转发到 `../@peri-sdk/scripts/generate-openui-spec.ts` 生成/校验
  `../@peri-sdk/src/openui/spec.json`；`build` 前置 check。
- `./node_modules/.bin/vite build`：只构建前端；`bun run build` 额外执行 `prepare:wasm`。
- 服务端冒烟：`GET /api/monitor`、`GET /api/chats`、WS `/api/chats/:id/sync`；真实执行需模型端点与 Store。
- 边界回归：`scripts/module-boundary.test.ts` 扫描生产源码，强制依赖方向、禁止循环与自有 index 引用，并拦截旧命令/事件路由和旧门面。
- 浏览器包检查：`dist/client` 不应出现模型/Store 凭据变量、`node:fs` 或 `@tursodatabase`。
