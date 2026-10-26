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
```

- **平台装配**：`server/compose.ts` 是 server 内唯一构造具体 `ChatManager` 的位置，把 WASM 运行后端、
  Store、memory KV 与默认配置装配成 SDK 实现；`chatManagerOptions(config)` 是可直接断言的纯装配。
- **服务端宿主**：`server/host.ts` 的 `ChatHost` 持有 `ChatManagerContract` 与 `ChatViewRegistry`，
  转发 create/get/find/list，编排 close 与 shutdown；`createChat` 不启动 WASM，不读历史。
- **视图**：`ChatViewRegistry` 由 SDK 提供（主入口与 `/portable`，不在 `/view`），每个 Chat 一个
  `ChatView`；同一 Chat 的多个 WS 连接共享视图，断开连接不释放视图；绑定换代由 View 成对替换文档。
  `closeChat` 先关句柄再释放视图，`shutdown` 先 `disposeAll` 再关闭 SDK 端口。
- **HTTP**：`server/api/index.ts` 只做边界校验、目标绑定核对与错误映射；执行、绑定与关闭语义在 SDK。
- **WS + Yjs**：`server/api/sync.ts` 把 Bun `ServerWebSocket` 适配为 `ChatYjsChannel`；客户端只能发
  HELLO/ACK，任何文档更新帧都被拒绝；慢客户端由 SDK 交付信用/背压显式关闭。
- **前端**：`frontend/remote/controller.ts` 是页面控制器（句柄身份、两步打开历史、绑定代次、Action、
  连接）；`actions.ts` 是 HTTP 客户端；`sync.ts` 是 WebSocket → SDK 副本；`application.ts` 是前端组合根。
  `core/agent/view/projection.ts` 把 `ChatReplica` 物化为展示 DTO，不反向控制执行。
- **静态资源**：非 `/api/*` 路径由 Bun 入口提供 `dist/client` 与 `index.html` 回退；Vite 开发服务器把
  `/api` 代理到 Bun 服务。
- **没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。
- **单进程 ownership**：`server/compose.ts` 固定使用 `createStorage()`（unstorage memory driver）作为 `kv`，由 `ChatManager` 内部构造协调器；多实例部署需要改造装配接入共享 storage，并由路由层做串行化/崩溃清理，不能宣称
  跨实例接管。

## 路由

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/info` | 去敏后的服务信息 |
| GET | `/api/chats` | 只读 Store 目录；不可达返回 503 + `available:false`，不伪造空列表 |
| POST | `/api/chats` | 只登记句柄，返回 201 + `{chat:{id,sessionId:null,bindingRevision:0}}`；不新建 Session |
| POST | `/api/chats/:chatId/actions` | `ChatUserAction` + 目标绑定；202 回执带 `inputId`/`status`/`applied` 等纯数据 |
| DELETE | `/api/chats/:chatId` | `closeChat`；成功 204；未确认关闭 500 + `incomplete`/`errors`；句柄已移除但 View 未释放时，同一 `chatId` 的 DELETE 会重试释放并返回 204 |
| GET | `/api/chats/:chatId/payload/:refId?bytes=` | 当前激活的载荷仓库；错误映射为公开 payload code |
| WS | `/api/chats/:chatId/sync` | 只读 Yjs 同步；未知 Chat 在升级前 410 |

## 两步打开历史

1. `POST /api/chats` 取得空句柄（`sessionId:null`，`bindingRevision:0`）。
2. `POST .../actions` 发送 `change_session`；202 回执给出真实 Session ID 与新 `bindingRevision`。
3. 只有绑定成功后，前端才为该句柄打开 `/sync` WebSocket 并等待首屏 SNAPSHOT。
4. 若服务端已有该 Session 的控制者，`change_session` 返回 `binding_conflict`；前端释放候选句柄，改用
   Session ID 寻址既有句柄，不重复绑定。切换页面只释放本地连接，不 cancel、不 close。
5. 新会话走 `create_new_session`，同样等待 202 后才连接；失败时清理候选句柄。

## 错误映射

- 加载路径已有控制者：409 `binding_conflict`。
- 加载路径 KV 读/写失败：500 `claim_failed`。
- 新会话 claim 阶段失败（本地/KV 冲突或 KV 读/写失败）：500 `session_creation_failed`（stage `claim`）。
- 没有通用 503 ownership 错误码；`closed` 仍是独立的 503。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| Bun 入口（HTTP + 静态资源 + WS + 信号） | `server/index.ts` |
| 环境变量解析与启动校验 | `server/config.ts` |
| 平台装配（唯一 `new ChatManager`） | `server/compose.ts` |
| 服务端宿主（句柄转发 + View 生命周期） | `server/host.ts` |
| HTTP 路由与错误映射 | `server/api/index.ts`、`server/api/errors.ts` |
| WebSocket 同步适配 | `server/api/sync.ts` |
| 共享 Zod DTO / HTTP 契约 | `core/shared/chat.ts`、`core/shared/sync.ts`、`core/shared/chat-display-state.ts` |
| 浏览器投影与问答适配 | `core/agent/view/projection.ts`、`core/agent/view/askUserQuestion.ts` |
| 远程客户端与页面控制器 | `frontend/remote/{application,actions,controller,repository,sync}.ts` |
| Chat 运行时与 useChat | `frontend/chat/runtime/useChat.ts`、`frontend/chat/runtime/queries.ts`、`frontend/chat/runtime/payloadReader.ts` |
| Chat 展示与交互 | `frontend/chat/{components,timeline,composer}/`；`frontend/components/assistant-ui/` 只保留 vendored 审批/问答卡片 |
| SPA 外壳与只读服务信息 | `frontend/app/App.tsx`、`frontend/settings/ServerInfoDialog.tsx` |
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
- `bun test`：`core`、`frontend`、`scripts`，再以 server tsconfig 跑 `server`。
- `./node_modules/.bin/vite build`：只构建前端；`bun run build` 额外执行 `prepare:wasm`。
- 服务端冒烟：`GET /api/info`、`GET /api/chats`、WS `/api/chats/:id/sync`；真实执行需模型端点与 Store。
- 边界回归：`core/agent/boundary.test.ts` 扫描生产源码，禁止旧命令/事件路由、旧门面和跨层引用。
- 浏览器包检查：`dist/client` 不应出现模型/Store 凭据变量、`node:fs` 或 `@tursodatabase`。
