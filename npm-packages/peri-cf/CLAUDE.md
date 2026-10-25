# Peri CF

## Scope

独立的前后端分离 Chat 应用，不属于根 Cargo workspace。Bun 长驻服务承载 SDK Chat 控制面与 Peri
ACP Host；React/Vite 前端只消费 HTTP Action 与 WebSocket + Yjs 只读副本。按需读取
`../../docs/standards/index.md`；跨层与安全变更读 architecture-contracts，测试读 testing，文档读
documentation。结构见 `../../docs/design/peri-cf-server-agent.md` 与
`../../docs/design/peri-sdk-chat.md`。

## 边界

**服务端是唯一计算实例**：`server/index.ts` 启动 Bun 进程；`server/agent/manager.ts` 的
`AppChatManager` 是组合根，持有 SDK `ChatManager` 与 `ChatViewRegistry`；句柄、绑定、Action、事实与
关闭语义都在 SDK。浏览器不启动 WASM 宿主，刷新/关闭页面不结束执行；服务进程退出会结束在途执行，
历史仍在 Store。

**凭据与工具面只在服务端**：模型、Store、machineId 与远程 MCP 全部来自环境变量
（`server/config.ts`）；前端没有 localStorage 凭据、relay 或首次运行门禁。`PERI_MCP_SERVERS` 只接受
远程 Streamable HTTP；不启动 local-fs、模拟 Bash、stdio 或进程内 builtin。`/api/info` 只返回去敏地址。

**没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。machineId 必须是
部署方提供的持久 UUID，服务端不随机生成。

**唯一持久权威是 Turso/libSQL 的 Rust Store**；Yjs 展示态与运行元数据只在内存。新建会话经 ACP
`session/new` + `session/rename`，默认系统指令（`server/system-instructions.ts`，经
`_meta["peri.instructions"]`）只对新会话生效；加载历史使用 Store 中的持久 cwd。

## 代码布局

- `core/shared/`：前后端共用 Zod 契约与展示 DTO（`chat.ts`、`sync.ts`、`chat-display-state.ts`）与
  WASM 构建常量。
- `core/agent/`：平台无关展示内核（不依赖 React/DOM/localStorage）。`view/projection.ts` 持有
  `ChatReplica` 并物化 `ChatDisplayState`；`view/askUserQuestion.ts` 做问答适配。
- `server/`：`config.ts`（环境变量与启动校验）、`agent/manager.ts`（SDK Manager + View 注册表）、
  `agent/views.ts`（每 Chat 一个 ChatView）、`store.ts`（Store 只读适配与缺表容忍）、`api/index.ts`
  （Hono 目录/句柄/Action/payload）、`api/sync.ts`（WS → ChatYjsChannel）。
- `frontend/remote/`：HTTP Action、WebSocket 客户端、页面控制器、目录仓储与组合根。
- `frontend/chat/`、`frontend/components/`、`frontend/app/`：assistant-ui 适配与展示外壳；
  `frontend/settings/` 只读服务端信息。
- `scripts/`：WASM 构建/provenance、开发启动与探针；`wasm/generated/` 是构建产物（gitignored），
  只由服务端加载。

模块规则：文件夹是模块，目录根 `index.ts` 是唯一公共出口；`core/agent/boundary.test.ts` 扫描生产
代码，强制 `frontend/` 与 `server/` 互不引用，`core/agent` 不得引入浏览器/React 依赖。

## 数据流与生命周期

- `GET /api/chats` 只读 Store 目录；`POST /api/chats` 只登记空句柄；`POST /api/chats/:id/actions`
  执行 `ChatUserAction`；`DELETE` 关闭句柄；`GET .../payload/:refId?bytes=` 读取全文。
- 打开历史是两步：先登记空句柄，再 `change_session`；202 回执给出真实 Session ID 与绑定代次后才连
  `WS /api/chats/:id/sync`。已有控制者返回 `binding_conflict`，前端改用 Session ID 寻址既有句柄。
- 同步只读：客户端只发 HELLO/ACK，服务端发 SNAPSHOT/UPDATE；不能写 Y.Doc。连接断开不停止执行，
  重连用 HELLO resume 补齐；慢客户端由 SDK 交付信用/背压显式关闭。
- 运行/交互/队列事实只来自选中会话的副本；列表不提供每会话运行态。
- 命令失败契约以 `disposition` 为唯一权威：`rejected` 明确未生效，`unknown` 执行状态未知（不得
  自动重试）；`not_sent` 只存在于浏览器预检。不要用 HTTP status 反推可重试性。
- `cancel` 的 `requested` 不是 stopped；关闭未确认会保留句柄、claim 与资源，可重试。

## 服务端配置

必填：`PERI_MODEL_ID`、`PERI_MODEL_BASE_URL`、`PERI_MODEL_API_KEY`、`PERI_STORAGE_URL`、
`PERI_STORAGE_TOKEN`、`PERI_MACHINE_ID`。可选：`PERI_MODEL_PROVIDER`、`PERI_MCP_SERVERS`、
`PERI_SERVER_HOST`、`PERI_SERVER_PORT`、`PERI_WASM_MODULE_URL`、`PERI_STATIC_ROOT`。任一解析问题
拒绝启动；只有 `LANGFUSE_*` 传入 WASM 实例。

## Chat 前端（assistant-ui）

设计见 `../../docs/design/peri-cf-chat-ui.md`；状态权威是 SDK `/view` 的 `SessionView` 经
`core/agent` 物化后的展示 DTO，前端不复制状态机。

- `frontend/chat/runtime/useChat.ts`：装配 `ChatController`、目录查询缓存与 payload reader。
- `frontend/chat/runtime/adapter.ts`：Entry → assistant-ui 消息的单向转换与回调接线；不提供
  编辑/重生成/队列/附件/语音 handler。
- `frontend/chat/components/`：消息流、工具卡片、会话列表、欢迎页与交互面板；`ChatShell` 组合侧栏、
  时间线、输入区与右面板，`PendingInteractions`/`InteractionCard` 承载交互，`ChatRightPanel` 汇总
  详情、计划与任务。
- `frontend/components/assistant-ui/`、`ui/`、`hooks/`、`lib/`：vendored registry 源码，上游升级
  手动同步；改动前读文件头定制记录。
- 样式：`frontend/tailwind.css` 是 Tailwind 入口与主题 token；`frontend/styles.css` 保留外壳与设置页。

## SDK 消费边界

只消费已发布 SDK 入口：主入口（`ChatManager`/`ChatService`/`ChatUserAction`/`ChatView` 契约）、
`/view`（只读副本、选择器、载荷读取、同步协议）、`/wasm`（宿主启动、调度器端口、transport、关闭
证据）。不引用 `@peri-sdk/src`、不新增 entry、不复制准入或同步规则。服务端只注入调度器端口；网络由
Emscripten fetch 执行器发起。SDK 不感知 assistant-ui。

`core/shared/chat.ts` 与 `sync.ts` 的 Zod schema 是 wire/展示 DTO 的单一来源。ACP 投影由 SDK
`ChatView`/`ChatReplica` 负责，应用只做展示适配。

**SDK dist 前置**：CF 通过 `file:../@peri-sdk` 解析 `node_modules/@peri-code/sdk`。干净 CI 先在 SDK
执行 `MISE_ENV=wasm mise exec -- bun run build`，再在 CF 执行 `bun install --force`（file 依赖变为
指向工作区的符号链接）；否则会使用旧 dist，表现为导出缺失或类型漂移。SDK build 依赖 Emscripten
6.0.10 与 peri-wasm 产物，工具链不满足时构建失败，不能用旧 dist 冒充。

## 路由与命令

- `bun run dev`：`prepare:wasm` 后同时启动 Bun 服务（watch）与 Vite（`/api` 代理到 8787）。
- `bun run dev:server` / `dev:web`：分开启动。
- `bun run start`：生产启动 Bun 服务（需环境变量与已构建资源）。
- `bun run typecheck`、`bun test`、`./node_modules/.bin/vite build`：TS、行为测试与前端构建。
- `bun run build`：`prepare:wasm` + Vite；`build:wasm` 构建 CF 专用 profile 与 provenance；
  `prepare:wasm` 只校验并原子发布到 `wasm/generated`。`core/shared/runtime-limits.ts` 属于 wasm
  构建输入。

默认没有本地文件/终端工具；远程能力需经 `PERI_MCP_SERVERS` 单独配置。单进程 `MemoryKV` 不能宣称
多实例接管；工具环境、真实模型/Store 与部署端资源仍需环境验收。
