# Peri CF

## Scope

独立的前后端分离聊天应用，不属于根 Cargo workspace。Bun 长驻服务承载 Peri ACP Host 与会话运行期；React/Vite 前端只消费 HTTP/SSE。按需读取 `../../docs/standards/index.md`，跨层与安全变更读取 architecture-contracts，测试读取 testing，文档读取 documentation。目标设计见 `../../docs/design/peri-cf-server-agent.md`。

## 边界

**服务端是唯一计算实例**：`server/index.ts` 启动 Bun 进程；`ChatManager` 是唯一入口与组合根，装配 `TursoChatSessionStore`、宿主启动器与环境工厂，向下传给 `ChatService`；`ChatService` 驻留 `PeriInstance` 并承载投影、命令与交互。浏览器不再启动 WASM 宿主，刷新/关闭页面不结束执行；服务进程退出会结束在途执行，历史仍在 Store。

**凭据与工具面只在服务端**：模型、Store、machineId 与远程 MCP 全部来自环境变量（`server/config.ts`）；前端没有 localStorage 凭据、relay 或首次运行门禁。`PERI_MCP_SERVERS` 只接受远程 Streamable HTTP；不启动 local-fs、模拟 Bash、stdio 或进程内 builtin。`/api/info` 只返回去敏地址。

**没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。machineId 必须是部署方提供的持久 UUID，服务端不随机生成。

**唯一持久权威是 Turso/libSQL 的 Rust Store**；投影与运行元数据只在服务端内存。新建会话经一次性宿主的 ACP `session/new` + `session/rename`，`session/new` 携带默认系统指令（`server/system-instructions.ts`，经 `_meta["peri.instructions"]`），随会话快照冻结、只对新会话生效。

## 代码布局

- `core/agent/`：平台无关会话内核（不依赖 React/DOM/localStorage）。`types.ts` 定义 `ChatSessionStore` / `ChatRepository` / `StartHost` / `AgentEnvironment`；`session/`（`PeriInstance`、运行期端口、创建、defer）、`view/`（投影、交互、AskUserQuestion）、`transport/`（ACP 初始化）。会话运行期端口只暴露 `watch/read/command/dispose`，订阅与初始快照的顺序由实现负责。
- `core/shared/`：前后端共用 Zod 契约与展示 DTO（`chat.ts`、`sync.ts`、`sync-state.ts`）与 WASM 构建常量。
- `server/`：`config.ts`（环境变量与启动校验）、`manager.ts`（组合根：Store、WASM 宿主启动器、每实例远程 MCP 环境与 `ChatService` 装配）、`service.ts`（会话服务：`PeriInstance` 驻留与 LRU）、`store.ts`（Store 只读适配与缺表容忍）、`api/`（Hono 路由、命令校验、SSE）。
- `frontend/remote/`：HTTP 仓储、`RemoteChatRuntime`（SSE 投影 + POST 命令）、`client.ts` 的 `AgentManager` 浏览器门面与 `createRemoteAgent()`。
- `frontend/chat/`、`frontend/components/`、`frontend/app/`：assistant-ui 适配与展示外壳；`frontend/settings/` 只读服务端信息。
- `scripts/`：WASM 构建/provenance、开发启动与探针；`wasm/generated/` 是构建产物（gitignored），只由服务端加载。

模块规则：文件夹是模块，目录根 `index.ts` 是唯一公共出口；`core/agent/boundary.test.ts` 扫描生产代码，强制 `frontend/` 与 `server/` 互不引用，`core/agent` 不得引入浏览器/React 依赖。

## 数据流与生命周期

- `GET /api/chats` / `POST /api/chats` → `ChatManager` → `ChatService` → `TursoChatSessionStore`；创建走一次性 `SessionCreator`，不经过 Store 创建。
- 前端打开会话：HTTP `GET /api/chats/:id` 取完整 `SyncState`，随后 `AgentManager` 对同一个 `RemoteChatRuntime` 调用 `watch` 订阅 SSE、用 POST 提交命令。
- 服务端 `ChatService.watch` 直接委托 `PeriInstance.watch`：实现内部先订阅投影再 `read()`，保证首帧后不丢变化；SSE 每帧发送完整 `SyncState`，50 ms 合并，15 s 心跳；重连后重发完整快照。
- 复原纪律：**先建 runtime/订阅，再触发 `session/load`**，不得丢弃重放通知；`initialize` 声明 `peri.historyPagination`、`peri.replay`、`peri.unstableEvent`、`peri.userInputQueue`。
- 宿主按需启动：每次执行/复原一个实例，用完关闭；`ChatManager` 登记所有存活宿主，进程退出时由 `shutdown()` 先释放实例、再兜底关闭剩余宿主。关闭未确认会阻断该会话后续运行。
- 同会话只允许单轮执行。取消走会话级 `session/cancel` 通知，未确认停止必须阻断重跑。
- 命令失败契约以 `disposition` 为唯一权威：`rejected` 明确未生效（4xx），`unknown` 执行状态未知（500，不得自动重试）；客户端预检失败用 `not_sent`，只存在于浏览器侧，可安全重试。不要用 HTTP status 反推可重试性。
- `ChatService` 空闲 LRU 上限 8，执行中的实例不回收；客户端断开只关闭订阅。

## 服务端配置

必填：`PERI_MODEL_ID`、`PERI_MODEL_BASE_URL`、`PERI_MODEL_API_KEY`、`PERI_STORAGE_URL`、`PERI_STORAGE_TOKEN`、`PERI_MACHINE_ID`。可选：`PERI_MODEL_PROVIDER`、`PERI_MCP_SERVERS`、`PERI_SERVER_HOST`、`PERI_SERVER_PORT`、`PERI_WASM_MODULE_URL`、`PERI_STATIC_ROOT`。任一解析问题拒绝启动；`LANGFUSE_*` 传入 WASM 实例。

## Chat 前端（assistant-ui）

目标设计见 `../../docs/design/peri-cf-chat-ui.md`；状态权威为 `core/shared/sync.ts` + `core/agent/*`，前端不复制状态机。

- `frontend/chat/runtime/useChat.ts`：装配 `createRemoteAgent()`、订阅 `frontend/remote/client.ts` 的 `AgentManager` 快照、转发命令，并用 TanStack Query 缓存会话列表。
- `frontend/chat/runtime/adapter.ts`：投影 Entry → assistant-ui 消息的单向转换与回调接线；不提供编辑/重生成/队列/附件/语音 handler。
- `frontend/chat/components/`：消息流、工具卡片、会话列表、欢迎页与交互面板；`ChatShell` 组合侧栏、时间线、输入区与右面板，`PendingInteractions`/`InteractionCard` 承载交互，`ChatRightPanel` 汇总详情、计划与任务。
- `frontend/components/assistant-ui/`、`ui/`、`hooks/`、`lib/`：vendored registry 源码，上游升级手动同步；改动前读文件头定制记录。
- 样式：`frontend/tailwind.css` 是 Tailwind 入口与主题 token；`frontend/styles.css` 保留外壳与设置页。

## SDK 消费边界

只消费已发布 SDK 入口：`@peri-code/sdk/wasm`（宿主启动、调度器端口、transport、关闭证据）、`/portable`（投影、Store 读取、ACP 初始化、交互规则）、`/view`（只读视图与载荷读取）。不引用 `@peri-sdk/src`、不新增 entry、不复制准入或同步规则。服务端只注入调度器端口；网络由 Emscripten fetch 执行器发起。SDK 不感知 assistant-ui。

`core/shared/chat.ts` 与 `sync.ts` 的 Zod schema 是 wire/展示 DTO 的单一来源（含 `chatCommandSchema`）。ACP 投影复用 SDK `SessionDocs`，展示经 SDK view 读取。

## 路由与命令

- `bun run dev`：`prepare:wasm` 后同时启动 Bun 服务（watch）与 Vite（`/api` 代理到 8787）。
- `bun run dev:server` / `dev:web`：分开启动。
- `bun run start`：生产启动 Bun 服务（需环境变量）。
- `bun run typecheck`、`bun run test`、`bun run build`：TS、行为测试与 `prepare:wasm` + Vite 构建。
- `build:wasm` 构建 CF 专用 profile 与 provenance；`prepare:wasm` 只校验并原子发布到 `wasm/generated`。`core/shared/runtime-limits.ts` 属于 wasm 构建输入。

默认没有本地文件/终端工具；远程能力需经 `PERI_MCP_SERVERS` 单独配置。工具环境与部署端资源尚未验收。
