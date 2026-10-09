# Peri CF 代码索引

状态：新增独立的 Cloudflare 聊天应用。生产部署、真实模型和远端存储仍需环境验收；不属于根 Cargo workspace。使用说明与测试命令由 [包 README](../../npm-packages/peri-cf/README.md) 维护。

## 边界与数据流

列表：浏览器 React → 同源 Hono API → SDK `TursoStorage` → Rust Store 的 `threads` 表。执行：Hono API → 按实际 ACP Session ID 定位 SQLite Durable Object → SDK WASM transport → Peri WASM Host → 模型与同一 Turso/libSQL Store。API 与 DO 路由共用鉴权和错误 middleware，聊天执行不负责 HTTP 路由匹配。

展示同步：ACP 通知 → SDK `SessionDocs` → `SessionDocSync` → 只读 WebSocket → 浏览器 `SessionDocReplica` → SDK view。应用只适配 wire 编码、HTTP/WS 鉴权和展示元数据；断线重连恢复订阅，不重发命令或自动取消。

- 不设置目录 Durable Object，不保存独立聊天列表，也不在 TS 新建平行会话表。TS 复用 SDK 的元数据查询；新建经 ACP `session/new` 与 `session/rename` 写入 Rust Store，然后从 Turso 读回。聊天 ID 就是实际 ACP Session ID。
- 每聊天 Durable Object 只保存展示历史和运行协调状态，元数据从 Turso 刷新；Rust Store 是 Peri 会话数据的权威。不依赖 D1，DO 与 Rust Store 不具有跨存储原子事务。展示历史仍是本应用 DO 内的流式投影，不宣称能从任意已有 Rust 会话重建全部展示消息。
- Workers 使用既有 SDK WASM transport，应用内 `worker/sdk/index.ts` 集中接入工作区 SDK 的可移植模块，不新增 SDK public entry 或构建目标，不加载 Bun/stdio Agent 管理入口。SDK 的唯一准入实现 `AdmissionCore` 与 DO durable registry/ledger adapter 负责 ticket、entry、settlement 和停止证据；本应用不是通用 `ManagedAgents` 或 child runtime 的 Cloudflare 移植，不宣称在途计算恢复或分布式接管。
- 实例监控页是只读视图：`GET /api/instances` 按并发上限扇出读取各聊天 DO 的采样器快照，不新增实例存储、不保留历史、不提供实例操作；单会话读取失败降级为该项，DO 回收后不显示实例，也不把观测当作存活探测。
- Bearer token 对应单一共享信任域，不提供多租户所有权隔离。模型与存储密钥只在服务端配置；前端 token 设置不携带提供商密钥。
- 工具默认关闭，不将 Emscripten 虚拟目录当作真实文件系统。未来接入工具需经 MCP 能力边界单独设计授权与生命周期。
- 停止通过 SDK 的 exact-target SessionControl；仅下一次显式发送可凭本应用保存的 Stop 控制代次恢复，控制权发生变化则拒绝恢复。部署须配置持久 `PERI_MACHINE_ID`，不使用跨部署共享的示例身份。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| SPA 与同源 Worker 开发构建 | `npm-packages/peri-cf/vite.config.ts` |
| Workers、DO 与静态资源绑定 | `npm-packages/peri-cf/wrangler.jsonc` |
| Workers 业务入口 | `npm-packages/peri-cf/worker/index.ts` |
| 前后端共享 Zod schema 与 DTO | `npm-packages/peri-cf/shared/chat.ts` |
| SDK 二进制同步帧的文本边界 | `npm-packages/peri-cf/shared/sync.ts` |
| SDK view 到 UI 的共享展示转换 | `npm-packages/peri-cf/shared/sync-state.ts` |
| Hono API、共享 middleware 与边界校验 | `npm-packages/peri-cf/worker/api/` |
| Hono JSON 校验与实际字节上限 | `npm-packages/peri-cf/worker/api/json.ts` |
| 聊天 DO 路由与运行状态 | `npm-packages/peri-cf/worker/chat/routes.ts`、`session.ts` |
| SDK ACP/Yjs 投影与持久展示恢复 | `npm-packages/peri-cf/worker/chat/projection.ts` |
| WS 首帧鉴权、Hibernation 订阅恢复与 ACK 信用预算 | `npm-packages/peri-cf/worker/chat/sync.ts`、`ws-delivery.ts`，DO 事件入口 `worker/index.ts` |
| SDK 准入及 exact-generation 停止收尾 | `npm-packages/peri-cf/worker/execution/dispatcher.ts` |
| DO 原子 registry/ledger 存储适配 | `npm-packages/peri-cf/worker/execution/storage.ts` |
| 工作区 SDK 接线唯一入口 | `npm-packages/peri-cf/worker/sdk/index.ts` |
| 静态 WASM 实例化与部署配置 | `npm-packages/peri-cf/worker/wasm/` |
| 每 Agent 实例资源查询 DTO 与线性内存采样 | `npm-packages/peri-cf/shared/resources.ts`、`worker/wasm/resources.ts`；`GET /api/chats/:id/resources` |
| 实例监控页的只读聚合与前端页面 | `npm-packages/peri-cf/worker/instances/collect.ts`、`shared/instances.ts`、`web/instances/`；`GET /api/instances`，入口见 `web/App.tsx` |
| Workers TCP/DNS/调度 glue 适配 | `npm-packages/peri-cf/scripts/worker-glue.ts`、`worker/wasm/dns.ts` |
| 仅开发服务的本机 DNS 解析 | `npm-packages/peri-cf/scripts/local-dns.ts` |
| 编译 Worker 的默认 Wrangler 本地启动 | `npm-packages/peri-cf/scripts/local-preview.ts`、`local-preview-command.ts` |
| 真实浏览器操作验收（无模型/数据库 mock） | `npm-packages/peri-cf/scripts/e2e.mjs` |
| 浏览器 HTTP 查询/命令 | `npm-packages/peri-cf/web/api/client.ts` |
| 浏览器 SDK Yjs 副本与 WS 生命周期 | `npm-packages/peri-cf/web/api/sync.ts` |
| 聊天展示与生成生命周期 | `npm-packages/peri-cf/web/chat/` |
| TanStack 私有读侧缓存与取消 | `npm-packages/peri-cf/web/chat/queries.ts` |
| Token 切换工作区隔离及按需加载展示 | `npm-packages/peri-cf/web/App.tsx` |
| Token 设置 | `npm-packages/peri-cf/web/settings/Settings.tsx` |
| TS 直连 Turso 元数据查询 | `npm-packages/peri-cf/worker/chat/repository.ts` |
| 新聊天 ACP 创建与命名 | `npm-packages/peri-cf/worker/chat/creation.ts` |
| 初始化能力与工作区身份 | `npm-packages/peri-cf/worker/chat/bootstrap.ts` |
| WASM 所有权、取消与 isolate 内存准入 | `npm-packages/peri-cf/worker/wasm/{lifecycle,budget,scheduler,network}.ts`、`shared/runtime-limits.ts` |
| CF 专用构建、provenance 与原子准备 | `npm-packages/peri-cf/scripts/{build-wasm,wasm-provenance,prepare-wasm}.ts` |
| 构建 profile 产物对比（不测 runtime CPU） | `npm-packages/peri-cf/scripts/profile-wasm.ts` |
| 原始诊断与独立展示限制 | `npm-packages/peri-cf/worker/api/http.ts` |
| 行为测试 | `npm-packages/peri-cf/tests/` |

WASM 产物来源及协议验证入口参见 [Peri WASM 索引](peri-wasm.md)，通用 SDK 执行管理契约参见 [TS SDK 索引](peri-ts-sdk.md)。
