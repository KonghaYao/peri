# Peri CF 代码索引

状态：新增独立的 Cloudflare 聊天应用。生产部署、真实模型和远端存储仍需环境验收；不属于根 Cargo workspace。使用说明与测试命令由 [包 README](../../npm-packages/peri-cf/README.md) 维护。

## 边界与数据流

列表：浏览器 React → 同源 Hono API → SDK `TursoStorage` → Rust Store 的 `threads` 表。执行：Hono API → 按实际 ACP Session ID 定位 SQLite Durable Object → SDK WASM transport → Peri WASM Host → 模型与同一 Turso/libSQL Store。API 与 DO 路由共用鉴权和错误 middleware，聊天执行不负责 HTTP 路由匹配。

展示同步：ACP 通知 → SDK `SessionDocs` → `SessionDocSync` → SDK 帧编解码与交付信用 → 只读 WebSocket → 浏览器 `SessionDocReplica` → SDK view。应用只适配 DO 持久化的 socket attachment、HTTP/WS 鉴权和展示元数据；断线重连恢复订阅，不重发命令或自动取消。

- 不设置目录 Durable Object，不保存独立聊天列表，也不在 TS 新建平行会话表。TS 复用 SDK 的元数据查询；新建经 ACP `session/new` 与 `session/rename` 写入 Rust Store，然后从 Turso 读回。聊天 ID 就是实际 ACP Session ID。
- 每聊天 Durable Object 只保存展示历史和运行协调状态，元数据从 Turso 刷新；Rust Store 是 Peri 会话数据的权威。不依赖 D1，DO 与 Rust Store 不具有跨存储原子事务。展示历史仍是本应用 DO 内的流式投影，不宣称能从任意已有 Rust 会话重建全部展示消息。
- Workers 只消费已发布 SDK 入口：`/wasm-host`（SDK 拥有宿主启动、端口所有权与关闭证据）、`/portable`（投影、同步 wire/交付语义、Store 读取）与 `/view`；应用内 `worker/sdk/index.ts` 是唯一接线点，不引用 `@peri-sdk/src`、不加载 Bun/stdio 管理入口。Cloudflare 平台适配（CF 产物与静态模块注入、isolate 容量、线性内存观测、DO socket attachment）保留在应用，并通过 SDK 的 `moduleFactory` / `ports` / `onLifecycle` 注入。执行归属 Rust Host：当前 ACP 没有执行准入与精确停止协议（`peri/execution/*`、`session/control*` 在 Rust 与 SDK 中均不存在），应用不实现 admission/ledger，只负责宿主生命周期与 `session/cancel`；本应用不是通用 `ManagedAgents` 或 child runtime 的 Cloudflare 移植，不宣称在途计算恢复或分布式接管。
- 实例监控页是只读视图：`GET /api/instances` 按并发上限扇出读取各聊天 DO 的采样器快照，不新增实例存储、不保留历史、不提供实例操作；单会话读取失败降级为该项，DO 回收后不显示实例，也不把观测当作存活探测。
- Bearer token 对应单一共享信任域，不提供多租户所有权隔离。模型与存储密钥只在服务端配置；前端 token 设置不携带提供商密钥。
- 工具默认关闭，不将 Emscripten 虚拟目录当作真实文件系统。未来接入工具需经 MCP 能力边界单独设计授权与生命周期。
- 取消已迁移到当前 ACP：应用向宿主发送会话级 `session/cancel` 通知，取消成立以 prompt 结算为 cancelled 且 ACP Host 关闭被确认为准；启动中尚未发送 prompt 时只跳过本轮。旧的 `peri/execution/*` 准入与 `session/control*` 精确停止/控制代次在 Rust 与 SDK 中都不存在，应用侧 dispatcher/ledger adapter 已删除，取消后的下一次显式发送就是新回合。部署须配置持久 `PERI_MACHINE_ID`，不使用跨部署共享的示例身份。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| SPA 与同源 Worker 开发构建 | `npm-packages/peri-cf/vite.config.ts` |
| Workers、DO 与静态资源绑定 | `npm-packages/peri-cf/wrangler.jsonc` |
| Workers 业务入口 | `npm-packages/peri-cf/worker/index.ts` |
| 前后端共享 Zod schema 与 DTO（Entry/Block/Tool/Turn/Interaction/任务/计划/队列，块式 `entries`） | `npm-packages/peri-cf/shared/chat.ts` |
| SyncState（应用运行元数据 + SDK view 物化条目/工具/轮次/面板）Zod schema | `npm-packages/peri-cf/shared/sync.ts`（帧编解码与交付信用在 SDK） |
| SDK view 到 UI 的共享展示转换（无 flatten、无 entryMetadata 回填；不支持 pair 抛 `UnsupportedSchemaError`） | `npm-packages/peri-cf/shared/sync-state.ts` |
| Hono API、共享 middleware 与边界校验 | `npm-packages/peri-cf/worker/api/` |
| Hono JSON 校验与实际字节上限；取消请求在响应前有界消费 body | `npm-packages/peri-cf/worker/api/json.ts` |
| 聊天 DO 路由、运行状态与 `session/cancel` 取消 | `npm-packages/peri-cf/worker/chat/routes.ts`、`session.ts` |
| SDK ACP/Yjs 投影与持久展示恢复（`state()` 与 HTTP 首屏/WS 同 DTO；导入替换文档对） | `npm-packages/peri-cf/worker/chat/projection.ts` |
| DO 记录适配（`recordVersion:2` 索引 + `payload:<id>:<n>` 1 MiB 分段、v1 转换与 `record:v1-backup`、孤儿/过期回收） | `npm-packages/peri-cf/worker/chat/record.ts` |
| 载荷全文鉴权端点与版本化入口 | `worker/chat/routes.ts` + `worker/api/router.ts`（`/api/v2/chats/...`；旧 chat 路径 426） |
| 反向交互往返（权限/提问） | ACP reverse request → `worker/chat/session.ts` 的 `setRequestHandler` → SDK 投影 → WS/view → 浏览器 `web/chat/Panels.tsx` + `interactionForm.ts` → 鉴权命令 `POST /api/v2/chats/:id/interactions/:interactionId` → `worker/chat/interactions.ts`（live 登记表）→ 原 ACP response；回答内容/令牌不进 Yjs |
| 能力声明与任务/配置源 | `worker/chat/bootstrap.ts`（`peri.userInputQueue`、`peri.unstableEvent`）、加载后 `seedConfig` 与 `session/bg-tasks` 快照修复入口 |
| WS 首帧鉴权、Hibernation 订阅恢复与 DO socket attachment | `npm-packages/peri-cf/worker/chat/sync.ts`、`ws-delivery.ts`（帧与交付信用来自 SDK），DO 事件入口 `worker/index.ts` |
| 工作区 SDK 接线唯一入口（`/portable`、`/wasm-host`、`/view`） | `npm-packages/peri-cf/worker/sdk/index.ts` |
| SDK 宿主的 Cloudflare 接线（模块注入、容量、实例观测） | `npm-packages/peri-cf/worker/wasm/` |
| 每 Agent 实例资源查询 DTO 与线性内存采样 | `npm-packages/peri-cf/shared/resources.ts`、`worker/wasm/resources.ts`；`GET /api/chats/:id/resources` |
| 实例监控页的只读聚合与前端页面 | `npm-packages/peri-cf/worker/instances/collect.ts`、`shared/instances.ts`、`web/instances/`；`GET /api/instances`，入口见 `web/App.tsx` |
| Workers glue 注入与部署 DNS override 校验 | `npm-packages/peri-cf/scripts/worker-glue.ts`、`worker/wasm/dns.ts` |
| 仅开发服务的本机 DNS 解析 | `npm-packages/peri-cf/scripts/local-dns.ts` |
| 编译 Worker 的默认 Wrangler 本地启动 | `npm-packages/peri-cf/scripts/local-preview.ts`、`local-preview-command.ts` |
| 真实浏览器操作验收（无模型/数据库 mock） | `npm-packages/peri-cf/scripts/e2e.mjs` |
| 浏览器 HTTP 查询/命令 | `npm-packages/peri-cf/web/api/client.ts` |
| 浏览器 SDK Yjs 副本与 WS 生命周期 | `npm-packages/peri-cf/web/api/sync.ts` |
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
| 行为测试 | `npm-packages/peri-cf/tests/`（真实 workerd：`real-workerd-ws.test.ts` 覆盖 WS 交付/休眠与 P5 闭环验收——载荷端点、权限往返、断宿主；`p3-migration-drill.test.ts` 覆盖迁移与回滚演练；`scripts/smoke.mjs` 为本机真实 WASM × workerd × sqld × 模型 fixture 闭环） |

WASM 产物来源及协议验证入口参见 [Peri WASM 索引](peri-wasm.md)，通用 SDK 执行管理契约参见 [TS SDK 索引](peri-ts-sdk.md)。

聊天展示对象契约见 [peri-cf 聊天对象契约](../design/peri-cf-chat-objects.md)；DO 展示持久化已按 P3 批准方案切换为 `recordVersion:2` 展示 checkpoint（含 7 天 `record:v1-backup` 回滚窗口），纯文本 `session` 键仅作迁移输入。
