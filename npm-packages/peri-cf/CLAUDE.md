# Peri CF

## Scope

独立的 React/Vite + Cloudflare Workers 聊天应用，不属于根 Cargo workspace。按需读取 `../../docs/standards/index.md`，跨层与安全变更读取 architecture-contracts，测试读取 testing，文档读取 documentation。

## 边界

浏览器经同源 HTTP/WS 入口访问 Worker isolate；HTTP 只承载无状态面（列表、创建、按需复原读取、peek 观测）与 WS 升级，**没有 Durable Object、没有数据库表、没有展示持久化**。`worker/api` 负责 HTTP 路由，`chat` 负责会话生命周期与 isolate 内 runtime 注册表，`wasm` 负责实例化与配置；前端按 `web/api`、`web/chat`、`web/settings` 分组。

传输边界：**同一条 `/api/v2/chats/:id/sync` WS 连接上，文本帧 = 应用控制帧（prompt / cancel / interaction 回答 / payload 读取与 `command_result` 回执），二进制帧 = SDK 文档同步帧（含交付 ACK）**。会话命令必须走 WS：执行驻留在接受连接的 isolate，HTTP 没有会话亲和性；原 HTTP 命令/载荷端点返回 426。控制帧 schema 与编解码在 `shared/sync.ts`，帧与交付信用仍归 SDK。

服务端只消费已发布 SDK 入口：`/wasm-host`（SDK 拥有的 Emscripten 宿主启动与关闭证据）、`/portable`（平台无关语义）与 `/view`。`worker/sdk/index.ts` 是唯一接线点，不引用 `@peri-sdk/src`、不新增 SDK entry、不在应用复制准入或同步规则。Cloudflare 平台适配（CF profile 产物、静态模块注入、isolate 容量与内存观测、WS 升级与保活 seam）保留在应用，但通过 SDK 的 `moduleFactory` / `ports` / `onLifecycle` 与应用侧 `SocketHost` 注入，不自行实现启动顺序、回调所有权或清理证据；缺口与移除条件见 README「SDK 消费边界与平台适配」。

`GET /api/v2/chats/:id` 是复原读：触发 ACP `session/load` 历史重放（宿主在响应前以 `session/update` 重放），重建展示态后返回与 WS 同一 DTO。重放纪律：**先建 runtime/订阅，再触发 `session/load`**，不得丢弃重放通知。

`GET /api/v2/chats/:id/resources` 在应用层观测每次 WASM Host 启动的独立身份、实际 SDK 代次与导出线性内存；不把 Worker 进程指标归属于 Agent。没有实例 CPU 计数时明确返回不可用，关闭后只保留最后观测。peek 语义：不触发复原、不启动 Host，本 isolate 无该会话内存态即无实例观测。

`GET /api/instances` 与前端“实例监控”页（`web/instances/`）只做只读扇出聚合：按固定并发上限 peek 各会话在本 isolate 的采样器，不新增实例存储、不保留历史、不提供实例操作；单个会话读取失败降级为该项，不伪装成“没有实例”，也不把观测当作存活探测。

列表由 TS 经 SDK `TursoStorage` 直接查询外部 Turso/libSQL 的 Rust Store，不设置目录 DO，不复制聊天列表或维护平行会话表。新聊天经 ACP 创建和命名，再从 Store 查询读回；聊天 ID 就是实际 ACP Session ID。**唯一跨 isolate 持久权威是 Turso Rust Store**：Yjs 展示态、运行元数据、连接状态只在 isolate 内存，isolate 回收即丢失并按需重放复原。共享 Bearer token 是一个信任域，不是多租户身份。模型与存储密钥禁止进入浏览器或 `VITE_*` 环境变量。

同聊天只允许单轮执行。取消必须等待真实 ACP Host 关闭及状态落盘，未确认停止应阻断重跑；该阻断只在内存中，isolate 回收后不保留。**在途执行随 isolate 回收丢失且不复原**，展示历史不得重造 Rust 工作义务。

`shared/chat.ts` 的 Zod schema 是前后端 DTO 的单一来源，`shared/sync.ts` 同时保留展示 DTO 与控制帧 schema。HTTP 边界复用 Hono 鉴权、按实际字节限制请求体和 validator。ACP 投影复用 SDK `SessionDocs`；WebSocket 同步复用 SDK `SessionDocSync`、`SessionDocReplica`、帧编解码与交付信用，不自写文本累加、序号、状态向量或投递编号规则。展示通过 SDK view 读取，Yjs app metadata 不重复保存完整文本；浏览器副本只读，发送/取消/回答/载荷读取全走 WS 控制帧。WS 首帧鉴权前不发送文档，令牌不进入 URL；断线仅恢复订阅，不自动取消、不重放未确认命令。

TanStack Query 只管理读侧查询；每认证工作区持有独立客户端，token 切换须卸载旧工作区并关闭旧连接、取消查询、清空缓存。禁用自动查询重试/重取，不把发送或停止接入自动重放；执行与取消领域状态机保持显式。命令结果只在服务端明确答否时确定失败；断线、超时、未同步视为未确认，调用方必须阻断重跑。

取消走会话级 `session/cancel` 通知：没有执行准入、精确停止目标或控制代次，取消成立以 prompt 结算为 cancelled 且宿主关闭被确认为准；未确认关闭会在本 isolate 内阻断该会话后续运行。`PERI_MACHINE_ID` 是部署的持久 UUID，重启时不得重新生成。

## 路由与命令

入口及部署条件见 `README.md` 与 `../../docs/code-index/peri-cf.md`。

- `bun run test`：本包行为与展示测试。
- `bun run dev`：构建后在本地 workerd 启动真实 Worker，默认 8791；Vite HMR 单独用 `dev:hmr`，其外部存储 I/O 路径不作为完整 WASM 验收入口。
- `bun run typecheck`、`bun run build`：TS 与完整 CF/前端构建。
- `SQLD_BIN=/path/to/sqld bun run smoke:local`：真实本地 workerd/WASM、模型 fixture 和 sqld 闭环。
- `build:wasm` 通过仓库脚本构建 CF 专用 feature/profile 及 provenance；`prepare:wasm` 只校验并原子发布匹配当前源码的应用缓存，不复用 SDK 默认二进制，不在轻量准备中隐式编译 Rust。

部署无持久化原语：无 DO 绑定、无 migrations、无表；部署端尚未验收（免费档 CPU、长时执行保活、远程 Turso 地址与令牌）。生产部署需要服务端 secrets 和实际套餐资源验收。默认不接本地文件工具；远程能力需经 MCP 单独设计授权。
