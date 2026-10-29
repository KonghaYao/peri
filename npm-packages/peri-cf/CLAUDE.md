# Peri CF

## Scope

独立的前后端分离 Chat 应用，不属于根 Cargo workspace。Bun 长驻服务承载 SDK Chat 控制面与 Peri
ACP Host；React/Vite 前端只消费 HTTP Action 与 WebSocket + Yjs 只读副本。按需读取
`../../docs/standards/index.md`；跨层与安全变更读 architecture-contracts，测试读 testing，文档读
documentation。结构见 `../../docs/design/peri-cf-server-agent.md` 与
`../../docs/design/peri-sdk-chat.md`。

## 边界

**服务端是唯一计算实例**：`server/index.ts` 启动 Bun 进程；`server/compose.ts` 把平台依赖
（WASM 运行后端、Store、memory KV、默认配置）装配成 SDK `ChatManager` 的构造选项，入口用这些选项
构造具体实现；`server/registry.ts` 的 `ChatRegistry` 持有 `ChatManagerContract` 与
`ChatViewRegistry`，编排 View 释放与关闭/停机。句柄、绑定、Action、事实与关闭语义都在 SDK。
浏览器不启动 WASM 运行后端，刷新/关闭页面不结束执行；服务进程退出会结束在途执行，历史仍在 Store。

**凭据与工具面只在服务端**：模型、Store、machineId 与远程 MCP 全部来自环境变量
（`server/config.ts`）；前端没有 localStorage 凭据、relay 或首次运行门禁。`PERI_MCP_SERVERS` 只接受
远程 Streamable HTTP；不启动 local-fs、模拟 Bash、stdio 或进程内 builtin。`/api/monitor` 只返回去敏地址、运行中 sandbox、进程级内存事实，以及每个 WASM 实例的线性内存容量与数值型业务计数（不含消息/工具内容）。

**没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。machineId 必须是
部署方提供的持久 UUID，服务端不随机生成。

**唯一持久权威是 Turso/libSQL 的 Rust Store**；Yjs 展示态与运行元数据只在内存。新建会话经 ACP
`session/new` + `session/rename`，默认系统指令（`server/system-instructions.ts` = BASE + OpenUI 段，经
`_meta["peri.instructions"]`）只对新会话生效；旧会话不会获得 OpenUI 指令。加载历史使用 Store 中的持久 cwd。

## 代码布局

- `frontend/remote/`：HTTP Action、WebSocket 客户端、页面控制器、目录仓储与组合根；同目录承载
  `contracts.ts`（Zod wire/展示 DTO 与 `MonitorSnapshot`）、`display-state.ts`（`ChatDisplayState` 物化）、
  `projection.ts`（`ChatReplica` 投影）、`view-state.ts`（视图派生纯函数）、`delivery.ts`（投递结算）、
  `handles.ts`（句柄表）与 `logging.ts`。
- `server/`：`index.ts`（Hono 应用：API、静态资源与 WS 升级）、`config.ts`（环境变量与启动校验）、
  `compose.ts`（`chatManagerOptions` 平台装配）、`registry.ts`（`ChatRegistry`：View 生命周期与关闭/停机）、
  `api/index.ts`（Hono 目录/句柄/Action/payload）、`api/sync.ts`（WS → ChatYjsChannel）、
  `api/errors.ts`（错误映射）。
- `frontend/chat/`、`frontend/components/`、`frontend/app/`：Chat 展示外壳、时间线与交互卡片；
  `frontend/chat/runtime/askUserQuestion.ts` 承载问答表单适配；`frontend/settings/` 只读服务端监控。
- `frontend/chat/openui/`：只剩聊天适配层——`OpenUIMessage` 门面与懒加载
  `@peri-code/sdk/openui/renderer`、`actions` 纯函数、模块出口。组件库、spec、主题与围栏在
  `../@peri-sdk/src/openui`；库能力只从 `@peri-code/sdk/openui*` 四个入口取，不深层 import
  `@peri-code/sdk/src/openui/canvas|components|library`；服务端只经 `@peri-code/sdk/openui/prompt`
  读 `spec.json`，不 import React。OpenUI 模块不得 import timeline/components/runtime/composer。
- `scripts/`：WASM 构建/provenance、开发启动与探针；`wasm/generated/` 是构建产物（gitignored），
  只由服务端加载。

模块规则：文件夹是模块，目录根 `index.ts` 是唯一公共出口。例外是入口与内部实现目录：`frontend/app/`
由 `main.tsx` 直接启动、没有 barrel，`frontend/chat/timeline/rows/` 是 timeline 内部行组件，
`frontend/shims/` 是 Vite alias 的构建替身，三者都不设 `index.ts`。`scripts/module-boundary.test.ts`
扫描生产代码，强制依赖方向、禁止循环，并保证 `frontend/`、`server/`、`scripts/` 互不越界。

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
- `cancel` 的 `requested` 不是 stopped；关闭未确认会保留句柄、ownership 与资源，可重试。
- `DELETE` 先关 SDK 句柄再释放 View；进程 `shutdown` 先 `disposeAll` 释放全部 View，再关闭 SDK
  端口与 WASM 运行后端；任一段失败都上报并保留资源。

## 服务端配置

必填：`PERI_MODEL_ID`、`PERI_MODEL_BASE_URL`、`PERI_MODEL_API_KEY`、`PERI_STORAGE_URL`、
`PERI_STORAGE_TOKEN`、`PERI_MACHINE_ID`。可选：`PERI_MODEL_PROVIDER`、四档模型
`PERI_MODEL_ID_FABLE`/`_OPUS`/`_SONNET`/`_HAIKU`（缺省回退 `PERI_MODEL_ID`）、`PERI_MCP_SERVERS`、
`PERI_SERVER_HOST`、`PERI_SERVER_PORT`、`PERI_WASM_MODULE_URL`、`PERI_STATIC_ROOT`。任一解析问题
拒绝启动；只有 `peri-config` 实际读取的 Langfuse 键传入 WASM 实例，未读取的同前缀键（如
`LANGFUSE_TRACING`）不转发。

## Chat 前端

设计见 `../../docs/design/peri-cf-chat-ui.md`；状态权威是 SDK `/view` 的 `SessionView` 经
`frontend/remote` 物化后的展示 DTO，前端不复制状态机。

- `frontend/chat/runtime/useChat.ts`：装配 `ChatController`、目录查询缓存与 payload reader。
- `frontend/chat/components/`：消息流、工具卡片、会话列表、欢迎页与交互面板；`ChatShell` 组合侧栏、
  时间线、输入区与右面板，`shell/` 承载草稿槽 (`useComposerDrafts`)、导航编排 (`useChatNavigation`)
  与横幅 (`ChatBanners`)，`PendingInteractions`/`InteractionCard` 承载交互，`ChatRightPanel` 汇总
  详情、计划与任务。
- `frontend/chat/{timeline,composer}/`：Entry → 展示行与输入区；不提供编辑/重生成/队列/附件/语音
  handler。`composer/` 状态栏只读消费 `configOptions`/`tokenUsage`/`stateSnapshotMeta`，字数上限引用
  SDK `CHAT_SEND_TEXT_MAX_LENGTH`；草稿按会话持久化到版本化 localStorage；斜杠命令只读
  `availableCommands`；模型/模式/推理强度经 `ChatUserAction.set_config_option` 写入。
  `timeline/toolText.ts` 是折叠态与失败态共用的工具文本/退出码/流分段提取。assistant 行的
  `OpenUIMessage` 只在顶层完整 `openui` 围栏，或流结束后以
  `root = Stack(` 开头的裸程序时懒加载 Renderer；校验失败、引用未解析、`incomplete`、懒加载或
  渲染异常都回退 `StreamdownText`。复制按钮始终复制原始文本（含 DSL）。
- OpenUI 表单值 ephemeral；`Button` action 经 `parseOpenUIAction` 路由到 `chat.send` 或
  `window.open`（仅 http/https）。`actionsEnabled={canSend}` 在聊天忙时禁用交互。
- `frontend/components/assistant-ui/` 只保留 vendored 审批/问答卡片与样式常量；`frontend/hooks/`、
  `frontend/lib/` 提供共用工具。上游升级手动同步，改动前读文件头定制记录。
- 样式：`frontend/tailwind.css` 是 Tailwind 入口与主题 token；`frontend/styles.css` 只承担设置/监控
  外壳与全局基础样式，消息流在 `frontend/styles/`（chat token、用户气泡、Markdown、欢迎页）。
  深色主题由 `frontend/app/theme.ts` 把系统偏好同步成 `<html class="dark|light">`，Tailwind 与
  chat token 共用 `.dark`；首帧由 `index.html` 内联脚本设置，避免闪烁。

## SDK 消费边界

只消费已发布 SDK 入口：主入口（`ChatManager`/`ChatService`/`ChatUserAction`/`ChatView` 契约、
`ChatViewRegistry`、`ChatManagerContract`）、`/view`（只读副本、载荷读取、同步协议）、`/wasm`
（运行后端启动、transport、关闭证据）。不引用 `@peri-sdk/src`、不新增 entry、不复制准入或
同步规则。调度器与清理/启动超时由 SDK 内部自持，服务端不注入；网络由 Emscripten fetch 执行器发起。
SDK 不感知 assistant-ui。

`frontend/remote/contracts.ts` 与 `display-state.ts` 的 Zod schema 是浏览器侧 wire/展示 DTO 的单一
来源。ACP 投影由 SDK `ChatView`/`ChatReplica` 负责，应用只做展示适配。

**生产测试场**：CF 依赖 registry 上的 `@peri-code/sdk` 固定版本，不以源码 alias 或 `file:` 链接消费
SDK；`server/tsconfig.json` 不映射 SDK 源码，`scripts/wasm-tools.ts` 从 `node_modules` 读取已安装包
并校验 `exports["./wasm"]`。升级 SDK 必须显式改版本号并重装，使发布产物（入口、类型、WASM 资源）
在 `bun run typecheck && bun test` 中被真实验证。`peri-sandbox` 未发布前用
`file:../peri-sandbox`，CF 通过 `@peri-code/sdk/sandbox` 的 `SandboxManager` 为每个实际会话启动
真实实例：新 Chat 先启动实例，历史会话按 Store cwd 找回同一实例，旧 `/workspace` 会话不启动。
`server/sandbox.ts` 持有按 `sandboxId` 引用计数的租约池，`server/registry.ts` 负责把 workspace 与
MCP 合并进 `createChat`，并在关闭/停机时释放；HTTP 只映射 `sandbox_start_failed`。
`server/sandbox-adapter.test.ts` 校验 adapter 与已发布 `/sandbox` 的契约。

## 路由与命令

- `bun run dev`：`prepare:wasm` 后同时启动 Bun 服务（watch）与 Vite（`/api` 代理到 8787）。
- `bun run dev:server` / `dev:web`：分开启动。
- `bun run start`：生产启动 Bun 服务（需环境变量与已构建资源）。
- `bun run typecheck`、`bun test`、`./node_modules/.bin/vite build`：TS、行为测试与前端构建。
- `bun run openui:spec` / `bun run openui:check`：转发到 `../@peri-sdk/scripts/generate-openui-spec.ts`
  生成/校验 `../@peri-sdk/src/openui/spec.json`；改 `../@peri-sdk/src/openui/library.tsx` 后必须重新生成并提交。
- `bun test` 前的 `bunfig.toml` test preload（`frontend/test/react-dedupe.ts`）为 `@peri-code/sdk`
  的 OpenUI 源码统一 React；浏览器构建由 `vite.config.ts` 的 `resolve.dedupe` 处理。
- `bun run build`：`openui:check` + `prepare:wasm` + Vite；`build:wasm` 构建 CF 专用 profile 与 provenance；
  `prepare:wasm` 只校验并原子发布到 `wasm/generated`。`scripts/wasm-profile.ts` 的内存常量属于 wasm
  构建输入。
- `bun install` 的根 `postinstall` 执行 `node_modules/peri-sandbox/scripts/patch-sdk.mjs`（MCP 2.4.0
  任务结果补丁）；使用 `--ignore-scripts` 时需手动执行。`@peri-code/sdk` 也是 `file:` 依赖，OpenUI
  源码的依赖需先在 `../@peri-sdk` 执行 `bun install`（CF 只链接包目录，不安装其自身依赖）。

工具面由远程 Streamable HTTP MCP 与会话 sandbox 的 MCP（名称 `sandbox`）组成；WASM 进程内没有
文件/终端 builtin。`workspace` 是 ACP 保留名，宿主会要求资源发现，sandbox 不实现 `resources/list`，
改名会让会话启动失败。单进程 memory storage 不能宣称多实例接管；工具环境、真实模型/Store 与部署端
资源仍需环境验收。
