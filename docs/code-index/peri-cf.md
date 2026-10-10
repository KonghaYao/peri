# Peri CF 代码索引

状态：独立的 Cloudflare 聊天应用，**无 Durable Object、零表、零持久化**：展示态按需由 ACP `session/load` 重放复原，会话命令走 WS 文本控制帧。生产部署、真实模型、远端存储与平台 CPU/保活仍需环境验收；不属于根 Cargo workspace。使用说明与测试命令由 [包 README](../../npm-packages/peri-cf/README.md) 维护，架构基线见 [聊天对象契约设计](../design/peri-cf-chat-objects.md) 与 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md)。

## 边界与数据流

列表：浏览器 React → 同源 Hono API → SDK `TursoStorage` → Rust Store 的 `threads` 表。执行：HTTP/WS 请求 → isolate 内 runtime 注册表按实际 ACP Session ID 取得 `ChatRuntime` → SDK WASM transport → Peri WASM Host → 模型与同一 Turso/libSQL Store。所有路由共用鉴权和错误 middleware。

展示复原：会话运行时先建订阅/投影，再以 ACP `session/load` 触发重放（宿主在响应前以 `session/update` 重放持久历史），重放通知直接进入 SDK 投影。展示同步：ACP 通知 → SDK `SessionDocs` → `SessionDocSync` → SDK 帧编解码与交付信用 → WebSocket（二进制帧）→ 浏览器 `SessionDocReplica` → SDK view。连接状态只在 isolate 内存（无 socket attachment、无 Hibernation）；断线重连恢复订阅并由重放重建，不重发命令或自动取消。

- 不设置目录 Durable Object，不保存独立聊天列表，也不在 TS 新建平行会话表。TS 复用 SDK 的元数据查询；新建经 ACP `session/new` 与 `session/rename` 写入 Rust Store，然后从 Turso 读回。聊天 ID 就是实际 ACP Session ID。
- 运行期注册表（`worker/chat/registry.ts`）只驻留 isolate 内存：`runtimeFor` 惰性创建、`peek` 只读不触发复原、超限按 LRU 回收空闲运行期。Rust Store 是唯一跨 isolate 持久权威；isolate 回收即在途执行丢失且不复原，展示态在下次读取/连接时重放重建。
- 命令通道：同一条 `/api/v2/chats/:id/sync` WS 连接上，文本帧 = 应用控制帧（prompt/cancel/interaction/payload 与 `command_result`），二进制帧 = SDK 文档同步与交付 ACK；HTTP 命令/载荷端点返回 426（HTTP 无会话亲和性）。控制帧 schema 在 `shared/sync.ts`。
- Workers 只消费已发布 SDK 入口：`/wasm-host`（SDK 拥有宿主启动、端口所有权与关闭证据）、`/portable`（投影、同步 wire/交付语义、Store 读取）与 `/view`；应用内 `worker/sdk/index.ts` 是唯一接线点，不引用 `@peri-sdk/src`、不加载 Bun/stdio 管理入口。Cloudflare 平台适配（CF 产物与静态模块注入、isolate 容量、线性内存观测、WS 升级/保活 seam）保留在应用，并通过 SDK 的 `moduleFactory` / `ports` / `onLifecycle` 与应用侧 `SocketHost` 注入。执行归属 Rust Host：当前 ACP 没有执行准入与精确停止协议（`peri/execution/*`、`session/control*` 在 Rust 与 SDK 中均不存在），应用不实现 admission/ledger，只负责宿主生命周期与 `session/cancel`；本应用不是通用 `ManagedAgents` 或 child runtime 的 Cloudflare 移植，不宣称在途计算恢复或分布式接管。
- 实例监控页是只读视图：`GET /api/instances` 按并发上限 peek 各会话在本 isolate 的采样器快照，不新增实例存储、不保留历史、不提供实例操作；单会话读取失败降级为该项，无该会话内存态即不显示实例，也不把观测当作存活探测。
- Bearer token 对应单一共享信任域，不提供多租户所有权隔离。模型与存储密钥只在服务端配置；前端 token 设置不携带提供商密钥。
- 工具默认关闭，不将 Emscripten 虚拟目录当作真实文件系统。未来接入工具需经 MCP 能力边界单独设计授权与生命周期。
- 取消已迁移到当前 ACP：应用向宿主发送会话级 `session/cancel` 通知，取消成立以 prompt 结算为 cancelled 且 ACP Host 关闭被确认为准；启动中尚未发送 prompt 时只跳过本轮。旧的 `peri/execution/*` 准入与 `session/control*` 精确停止/控制代次在 Rust 与 SDK 中都不存在，应用侧 dispatcher/ledger adapter 已删除，取消后的下一次显式发送就是新回合。部署须配置持久 `PERI_MACHINE_ID`，不使用跨部署共享的示例身份。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| SPA 与同源 Worker 开发构建 | `npm-packages/peri-cf/vite.config.ts` |
| Worker、静态资源与 vars 绑定（无 DO 绑定、无 migrations） | `npm-packages/peri-cf/wrangler.jsonc` |
| Workers 业务入口 | `npm-packages/peri-cf/worker/index.ts` |
| 前后端共享 Zod schema 与 DTO（Entry/Block/Tool/Turn/Interaction/任务/计划/队列，块式 `entries`） | `npm-packages/peri-cf/shared/chat.ts` |
| SyncState（应用运行元数据 + SDK view 物化条目/工具/轮次/面板）与控制帧（`chatCommandSchema`/`chatCommandResultSchema`/`parseControlFrame`）Zod schema | `npm-packages/peri-cf/shared/sync.ts`（帧编解码与交付信用在 SDK） |
| SDK view 到 UI 的共享展示转换（无 flatten、无 entryMetadata 回填；不支持 pair 抛 `UnsupportedSchemaError`） | `npm-packages/peri-cf/shared/sync-state.ts` |
| Hono API、共享 middleware 与边界校验 | `npm-packages/peri-cf/worker/api/` |
| Hono JSON 校验与实际字节上限；取消请求在响应前有界消费 body | `npm-packages/peri-cf/worker/api/json.ts` |
| 会话 HTTP 面（`GET /api/v2/chats/:id` 触发重放复原、`/resources` peek、命令端点 426）与 WS 升级 | `npm-packages/peri-cf/worker/chat/routes.ts`、`worker/api/router.ts` |
| 单会话运行期（`ChatRuntime`：hydrate 重放复原、执行、取消、交互、载荷） | `npm-packages/peri-cf/worker/chat/session.ts` |
| isolate 内运行期注册表（LRU、`runtimeFor`/`peek`、空闲回收；命令与会话亲和性的载体） | `npm-packages/peri-cf/worker/chat/registry.ts` |
| SDK ACP/Yjs 投影与展示物化（`state()` 与 HTTP 首屏/WS 同 DTO；文档对随运行期存活） | `npm-packages/peri-cf/worker/chat/projection.ts` |
| **已删除**：DO 记录适配器 `worker/chat/record.ts`（`recordVersion:2`、分段 payload、备份回滚）随「无 DO、零持久化」转向移除 | 历史见 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md) |
| 命令与载荷通道（WS 文本控制帧 op `prompt`/`cancel`/`interaction`/`payload` 与 `command_result`） | `worker/chat/sync.ts` 的 `control()` + `worker/chat/session.ts` 的 `run()`；HTTP 旧端点/命令端点 426 |
| 反向交互往返（权限/提问） | ACP reverse request → `worker/chat/session.ts` 的 `setRequestHandler` → SDK 投影 → WS/view → 浏览器 `web/chat/Panels.tsx` + `interactionForm.ts` → WS 控制帧 `op:"interaction"` → `worker/chat/interactions.ts`（live 登记表）→ 原 ACP response；回答内容/令牌不进 Yjs |
| 能力声明与任务/配置源 | `worker/chat/bootstrap.ts`（`peri.userInputQueue`、`peri.unstableEvent`）、加载后 `seedConfig` 与 `session/bg-tasks` 快照修复入口 |
| WS 首帧鉴权、无状态连接生命周期、控制帧派发与投递信用；升级/保活/时钟经 `SocketHost` seam（CF 用升级请求的 `waitUntil`；无 attachment/Hibernation） | `npm-packages/peri-cf/worker/chat/sync.ts`、`ws-delivery.ts`（帧与交付信用来自 SDK） |
| 工作区 SDK 接线唯一入口（`/portable`、`/wasm-host`、`/view`） | `npm-packages/peri-cf/worker/sdk/index.ts` |
| SDK 宿主的 Cloudflare 接线（模块注入、容量、实例观测） | `npm-packages/peri-cf/worker/wasm/` |
| 每 Agent 实例资源查询 DTO 与线性内存采样（peek，不触发复原） | `npm-packages/peri-cf/shared/resources.ts`、`worker/wasm/resources.ts`；`GET /api/v2/chats/:id/resources` |
| 实例监控页的只读聚合与前端页面 | `npm-packages/peri-cf/worker/instances/collect.ts`、`shared/instances.ts`、`web/instances/`；`GET /api/instances`，入口见 `web/App.tsx` |
| Workers glue 注入与部署 DNS override 校验 | `npm-packages/peri-cf/scripts/worker-glue.ts`、`worker/wasm/dns.ts` |
| 仅开发服务的本机 DNS 解析 | `npm-packages/peri-cf/scripts/local-dns.ts` |
| 编译 Worker 的默认 Wrangler 本地启动 | `npm-packages/peri-cf/scripts/local-preview.ts`、`local-preview-command.ts` |
| 真实浏览器操作验收（无模型/数据库 mock） | `npm-packages/peri-cf/scripts/e2e.mjs` |
| 浏览器 HTTP 只读查询（命令端点 426 提示刷新） | `npm-packages/peri-cf/web/api/client.ts` |
| 浏览器 SDK Yjs 副本、WS 生命周期与 `ChatCommandPort` 命令通道（结果按帧 id 匹配，未确认不重放） | `npm-packages/peri-cf/web/api/sync.ts` |
| 聊天展示与生成生命周期 | `npm-packages/peri-cf/web/chat/` |
| TanStack 私有读侧缓存与取消 | `npm-packages/peri-cf/web/chat/queries.ts` |
| Token 切换工作区隔离及按需加载展示 | `npm-packages/peri-cf/web/App.tsx` |
| Token 设置与浏览器持久化（`localStorage` 键 `peri.access-token`，保存空值即清除） | `npm-packages/peri-cf/web/settings/Settings.tsx`、`tokenStorage.ts` |
| TS 直连 Turso 元数据查询 | `npm-packages/peri-cf/worker/chat/repository.ts` |
| 新聊天 ACP 创建与命名 | `npm-packages/peri-cf/worker/chat/creation.ts` |
| 初始化能力与工作区身份 | `npm-packages/peri-cf/worker/chat/bootstrap.ts` |
| 宿主接线与 isolate 容量/实例观测（启动与关闭证据在 SDK） | `npm-packages/peri-cf/worker/wasm/{host,budget,resources}.ts`、`shared/runtime-limits.ts` |
| CF 专用构建、provenance 与原子准备（哈希范围 = peri-wasm 本地依赖闭包） | `npm-packages/peri-cf/scripts/{build-wasm,wasm-provenance,prepare-wasm}.ts` |
| 构建 profile 产物对比（不测 runtime CPU） | `npm-packages/peri-cf/scripts/profile-wasm.ts` |
| 原始诊断与独立展示限制 | `npm-packages/peri-cf/worker/api/http.ts` |
| 行为测试 | `npm-packages/peri-cf/tests/`（真实 workerd：`real-workerd-ws.test.ts` 覆盖 WS 交付、控制帧命令与断宿主；`hydrate-replay.test.ts` 覆盖重放复原；`scripts/smoke.mjs` 为本机真实 WASM × workerd × sqld × 模型 fixture 闭环；`record-store.test.ts`、`p3-migration-drill.test.ts` 已随零持久化删除） |

WASM 产物来源及协议验证入口参见 [Peri WASM 索引](peri-wasm.md)，通用 SDK 执行管理契约参见 [TS SDK 索引](peri-ts-sdk.md)。

聊天展示对象契约见 [peri-cf 聊天对象契约](../design/peri-cf-chat-objects.md)：2026-10-10 起为**无 DO、零表、零持久化**基线，展示态按需由 ACP 重放复原，命令走 WS 文本控制帧；原 DO checkpoint（`recordVersion:2`/分段 payload/备份回滚）作废，取代过程见 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md)。
