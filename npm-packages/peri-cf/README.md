# Peri CF

Peri 的前后端分离示例：**Bun 长驻服务承载 Agent 执行**，React + Vite 只做展示、命令转发与交互回答。

服务端在进程内启动 peri-wasm ACP Host（每个执行/复原一个实例），`ChatManager` 装配只读 Turso/libSQL 适配并交给 `ChatService` 驻留会话实例；投影、命令处理与交互队列位于服务端；浏览器通过 HTTP 提交命令、通过 SSE 接收投影。模型与存储凭据只存在于服务端环境变量，前端不再保存密钥。

这是待部署验收的单可信用户示例，不是已完成生产验证的服务。服务没有内置认证与多租户隔离，部署入口控制（反向代理鉴权或等效设施）是必要条件。

## 运行模型

- **服务端是唯一计算实例**：`server/` 的 `ChatManager` 装配 `TursoChatSessionStore`、宿主启动器与环境工厂，`ChatService` 驻留 `PeriInstance`；冷启动经 ACP `session/load` 重放历史，在途执行不因客户端断开而终止。
- **前端只消费投影**：`frontend/remote/client.ts` 的 `AgentManager` 仍是浏览器会话门面，注入的是 HTTP 仓储与远程运行期；命令经 `POST /api/chats/:id/commands`，投影经 `GET /api/chats/:id/events`（SSE，整帧快照，50 ms 合并推送）。一次会话选择只复用一个 `RemoteChatRuntime`。
- **刷新/关闭页面不再结束执行**：执行属于服务端；重新打开页面会重新订阅同一会话并收到当前完整投影。
- **工具面只有远程 MCP**：`McpMiddleware` + `ToolSearch` 开启，后端从 `PERI_MCP_SERVERS` 声明远程 Streamable HTTP server；不存在浏览器 local-fs、模拟 Bash、stdio 或进程内 builtin 工具。
- **命令语义不变**：单轮互斥、取消需宿主关闭确认、交互回答失败码稳定（`busy` / `blocked` / `settled` / `invalid_answer` / `unavailable` / `integrity_error` / `too_large` / `not_found` / `internal_error`）。失败以 `disposition` 表达执行是否生效：`rejected`（明确未生效）与 `unknown`（执行状态未知）；客户端预检失败另记 `not_sent`（未离开客户端，可安全重试）。

## 服务端配置

全部从进程环境变量读取；任一必填项缺失或非法都会拒绝启动。

| 变量 | 必填 | 说明 |
| --- | --- | --- |
| `PERI_MODEL_PROVIDER` | 否 | `openai`（默认）或 `anthropic` |
| `PERI_MODEL_ID` | 是 | 模型 ID |
| `PERI_MODEL_BASE_URL` | 是 | 模型端点，必须是 `http(s)` |
| `PERI_MODEL_API_KEY` | 是 | 模型密钥 |
| `PERI_STORAGE_URL` | 是 | libSQL/Turso HTTP 端点，必须是 `http(s)` |
| `PERI_STORAGE_TOKEN` | 是 | 存储凭据 |
| `PERI_MACHINE_ID` | 是 | 规范 UUID；Store 内的机器身份，必须持久不变 |
| `PERI_MCP_SERVERS` | 否 | JSON 数组：`[{"name","url","token?","headers?"}]`；只接受 `http(s)`，`token` 写成 `Authorization: Bearer` |
| `PERI_SERVER_HOST` | 否 | 默认 `127.0.0.1`；容器内监听可设 `0.0.0.0` |
| `PERI_SERVER_PORT` | 否 | 默认 `8787` |
| `PERI_WASM_MODULE_URL` | 否 | 默认 `wasm/generated/peri-wasm.js`；可给绝对路径、`file:` 或 `http(s)` URL |
| `PERI_STATIC_ROOT` | 否 | 默认 `dist/client`；不存在时只提供 API |

`LANGFUSE_*` 会传给 WASM 实例；其余环境变量不会进入模块环境。

## HTTP 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/info` | 去敏后的模型/存储/MCP 摘要；设置页只读展示 |
| GET | `/api/chats` | 会话清单 |
| POST | `/api/chats` | 创建会话并执行 `session/new` + `session/rename` |
| GET | `/api/chats/:id` | 复原并返回完整 `SyncState` |
| GET | `/api/chats/:id/events` | SSE：`event: state`（完整投影）、`event: ping`（15 s） |
| POST | `/api/chats/:id/commands` | `prompt` / `cancel` / `interaction` / `payload`；契约见 `core/shared/chat.ts` |

`:id` 必须是 UUID。命令失败返回 `{ok:false, code, message, disposition}`：明确未生效为 `409`（会话缺失为 `404`），`disposition:"rejected"`；执行状态未知为 `500`，`disposition:"unknown"`。格式错误返回 `400` 与 `disposition:"rejected"`。
非 `/api/*` 路径由同一进程提供 SPA 静态资源与 `index.html` 回退。

## 本地启动

```bash
bun install
bun run prepare:wasm   # 校验并发布 WASM 到 wasm/generated（需要 Rust/Emscripten 工具链）
bun run dev            # Bun 服务（watch）+ Vite，Vite 把 /api 代理到 127.0.0.1:8787
```

生产构建与启动：

```bash
bun run build          # prepare:wasm + vite build → dist/client
bun run start          # bun server/index.ts（需先导出上面的环境变量）
```

只改前端时可分开运行：`bun run dev:server` 与 `bun run dev:web`。
`bun run typecheck`、`bun run test` 是验证入口。

WASM 产物必须与当前源码一致：`prepare:wasm` 校验源码、锁文件、工具链、构建参数与产物哈希后原子发布；缺失、过期或混合产物直接失败。纳入哈希的输入包括 `peri-wasm` 及其本地依赖闭包、`Cargo.toml`/`Cargo.lock`、`patches/`、`.cargo/`、仓库脚本与 `core/shared/runtime-limits.ts`。

## 目录职责

模块规则：**文件夹是模块边界，目录根的 `index.ts` 是唯一公共出口**；跨模块只能引用出口，测试跟随模块放置。`core/agent/boundary.test.ts` 静态扫描生产代码，并强制 `frontend/` 与 `server/` 互不引用。

- `core/agent/`：平台无关的会话内核（不依赖 React/DOM/localStorage）。`types.ts` 定义平台端口；`session/`（`PeriInstance`、运行期端口、创建、defer）、`view/`（投影、交互、AskUserQuestion）、`transport/`（ACP 初始化）是子模块。
- `core/shared/`：前后端共用的 Zod 契约与展示 DTO（`chat.ts`、`sync.ts`、`sync-state.ts`）以及 WASM 构建输入常量。
- `server/`：Bun 组合根（`index.ts`）、环境配置（`config.ts`）、组合根与宿主/MCP 装配（`manager.ts`）、会话服务与驻留 LRU（`service.ts`）、Turso 只读适配（`store.ts`）与 Hono API（`api/`）。
- `frontend/remote/`：HTTP 仓储、SSE 运行期、`client.ts` 的浏览器 `AgentManager` 门面与 `createRemoteAgent()` 组合根。
- `frontend/chat/`：assistant-ui 适配、查询缓存、载荷读取、交互表单与消息组件。
- `frontend/settings/`：只读服务端信息弹层，不保存凭据。
- `frontend/app/`：SPA 入口与应用外壳；没有首次运行门禁。
- `frontend/components/`、`hooks/`、`lib/`、`shims/`、`styles/`：assistant-ui/shadcn 组件、通用工具、构建替身与样式。
- `scripts/`：WASM 构建/provenance、开发启动与探针；每个脚本是独立命令入口。
- `wasm/generated/`：构建产物（gitignored），只由服务端加载。

## SDK 消费边界

只消费已发布 SDK 入口，不引用 `@peri-sdk/src`、不新增 entry、不在应用复制准入或同步规则：

| 入口 | 用途 |
| --- | --- |
| `@peri-code/sdk/wasm` | 服务端 Emscripten 宿主启动、调度器端口、ACP transport、截止时间与关闭证据 |
| `@peri-code/sdk/portable` | 平台无关语义：ACP 初始化、`SessionDocs` 投影、`TursoStorage`、交互规则 |
| `@peri-code/sdk/view` | 只读视图与载荷读取 |

服务端宿主只注入调度器端口（`createSchedulerPort`）；网络由 WASM 的 Emscripten fetch 执行器发起，模型与 Store 直连配置的上游。SDK `/portable` 重导出的准入模块引用 `crypto`；浏览器构建用 `frontend/shims/nodeCrypto.ts` 替换为显式失败的替身，前端不消费准入面。

## WASM 限制

WASM 运行的是 ACP Host，不是本地终端工具环境：没有宿主项目文件系统、任意 shell/子进程或本地 builtin MCP 服务。文件、终端、Web、Artifact、Cron、Workflow 与 Skills 只有在远程 MCP 提供时存在。

每个 CF WASM 线性内存最大 64 MiB、栈 8 MiB、增长步长 1 MiB（`core/shared/runtime-limits.ts` 是构建输入）。命令型 hook 在 Emscripten 下明确报错，不静默放行。

## 已知限制

- 没有认证、授权、限流与多租户隔离；只适合单可信用户或受入口控制保护的部署。
- SSE 每帧发送完整 `SyncState`（50 ms 合并），长会话的带宽与序列化开销高于 Yjs 增量同步。
- 服务进程退出时 `ChatManager.shutdown()` 先释放驻留实例，再兜底关闭仍存活的宿主并结束在途执行；重启后会话历史仍在 Store，执行不恢复。
- `ChatService` 默认最多驻留 8 个空闲实例，超限按 LRU 回收；正在执行的实例不会被回收。
