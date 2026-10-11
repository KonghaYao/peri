# Peri CF 代码索引

状态：独立的 Cloudflare 聊天应用，**浏览器是唯一计算实例、Worker 零配置无状态**：Peri ACP Host（peri-wasm）在浏览器启动，会话运行时/投影/命令/交互/载荷读取全部驻留标签页；Worker 只提供静态资源与 relay 透传（无 env/secrets、无 Durable Object、零表、无同步状态），上游地址与凭据由浏览器本地配置携带。生产部署、真实浏览器宿主、远端存储与平台资源仍需环境验收；不属于根 Cargo workspace。使用说明与命令由 [包 README](../../npm-packages/peri-cf/README.md) 维护，架构基线见 [浏览器宿主 Agent 设计](../design/peri-cf-browser-agent.md)，迁移范围与验收见 [迁移 issue](../../spec/issues/2026-10-11-peri-cf-browser-agent-migration.md)。

## 边界与数据流

列表与创建：浏览器 React → 标签页应用门面（`web/runtime/application.ts`）→ SDK `TursoStorage`（同源 relay，上游基址与 token 来自浏览器本地配置）→ Rust Store。执行：内存命令 → 标签页 `ChatRegistry` 按实际 ACP Session ID 取得 `ChatRuntime` → SDK `startPeriWasmHost` → Peri WASM Host → 模型经 `/api/relay/model/*`、存储经 `/api/relay/storage/*`（Rust 侧 fetch 执行器；relay 按请求路径首段指定的上游转发）。

展示复原：会话运行期先建订阅/投影，再以 ACP `session/load` 触发重放（宿主在响应前以 `session/update` 重放持久历史），重放通知直接进入 SDK 投影；`state()` 与首屏读取返回同一 SyncState DTO。展示不再跨网络同步：Yjs 文档（`SessionDocs`）只存在于标签页内存，`readSessionView` 物化展示面。

- Worker 零配置：`apiApp()` 统一 no-store 与错误响应；relay 按路径首段（`/api/relay/<storage|model>/<encodeURIComponent(上游绝对基址)>/…`，基址须 `http(s)`）转发，凭据头原样上行、逐跳头与跳源头剥离，存储 JSON 响应 `base_url` 改写为 relay 前缀。除 relay 外 `/api/*` 一律 404（没有 `GET /api/config`）；非 API 路径走静态资源（SPA 回退）。
- 配置与凭据在浏览器：`web/settings/config.ts` 以 localStorage 单键 `peri-cf.browser-config.v1` 保存 machineId、模型与存储配置，`web/settings/SettingsView.tsx` 是首次运行门禁与标题栏入口；密钥经 relay 明文过网（DevTools 可见）。
- 运行期注册表（`web/runtime/registry.ts`）只驻留标签页内存：`runtimeFor` 惰性创建、超限按 LRU 回收空闲运行期（忙碌时允许超限）。Rust Store 是唯一持久权威；刷新/关闭标签页等价于宿主实例结束，在途执行丢失且不复原。
- 命令通道：内存调用（`web/runtime/session.ts` 的 `ChatCommand` → `ChatCommandError`），不再有 WS 控制帧；失败总是「确定未生效」。浏览器侧不实例化 SessionDocReplica/SessionDocSync。
- 只消费已发布 SDK 入口：`@peri-code/sdk/wasm`（宿主启动、调度器端口、transport、截止时间与关闭证据）、`/portable`（投影、Store 读取、交互规则）、`/view`（只读视图与载荷读取）；不引用 `@peri-sdk/src`、不加载 Bun/stdio 管理入口。SDK `/portable` 的准入模块引用 `crypto`，浏览器构建以显式失败的替身替换（`web/shims/nodeCrypto.ts` + Vite alias）。
- 取消：会话级 `session/cancel` 通知，取消成立以 prompt 结算为 cancelled 且宿主关闭被确认为准；未确认关闭在标签页内阻断该会话后续运行。当前 ACP 没有执行准入与精确停止协议，应用侧不实现 admission/ledger。
- 没有访问控制：HTTP 面与 relay 对知道 URL 的任何人开放，不提供多租户隔离；relay 不限制转发目标，未挂入口控制时等价开放转发面（已知风险，代码不做限制）。部署入口控制由 Cloudflare Access 或等效设施承担。machineId 是客户端身份：首次运行 `crypto.randomUUID()` 生成并长期保存（须为规范 UUID），清除即换身份。
- 工具默认关闭，不将 Emscripten 虚拟目录当作真实文件系统；接入远程 MCP 需按能力边界单独设计授权与生命周期。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| SPA/Worker 开发构建与 `crypto` 替身 alias | `npm-packages/peri-cf/vite.config.ts`、`web/shims/nodeCrypto.ts` |
| Worker、静态资源与 `ASSETS` 绑定（无 vars、无 DO 绑定、无 migrations） | `npm-packages/peri-cf/wrangler.jsonc` |
| Worker 入口（ASSETS + `/api/*` 分发） | `npm-packages/peri-cf/worker/index.ts` |
| 无状态 API 装配（relay 注册） | `npm-packages/peri-cf/worker/api/router.ts` |
| relay 透传（目标解析、头过滤、流式转发、`base_url` 改写） | `npm-packages/peri-cf/worker/api/relay.ts` |
| 统一 no-store/错误/404 语义；错误诊断与响应 | `npm-packages/peri-cf/worker/api/app.ts`、`http.ts` |
| Worker 部署环境契约（仅 `ASSETS` 绑定） | `npm-packages/peri-cf/worker/types.ts` |
| 标签页应用装配（origin、startHost、registry、repository） | `npm-packages/peri-cf/web/runtime/application.ts` |
| 浏览器宿主启动（模块工厂 + 调度器端口） | `npm-packages/peri-cf/web/host/index.ts`、`module.ts` |
| 浏览器宿主运行配置（上游基址编码进 relay 路径、`/workspace`、BareHarnessConfig） | `npm-packages/peri-cf/web/host/config.ts` |
| 浏览器本地配置读写（localStorage 单键、必填校验、machineId 生成） | `npm-packages/peri-cf/web/settings/config.ts` |
| 设置页（首次运行门禁、标题栏齿轮入口、保存后刷新生效） | `npm-packages/peri-cf/web/settings/SettingsView.tsx`、`web/App.tsx` |
| 单会话运行期（`ChatRuntime`：hydrate 重放复原、执行、取消、交互、载荷） | `npm-packages/peri-cf/web/runtime/session.ts` |
| 标签页运行期注册表（LRU、`runtimeFor`） | `npm-packages/peri-cf/web/runtime/registry.ts` |
| SDK 投影与展示物化（`state()`；文档对随运行期存活） | `npm-packages/peri-cf/web/runtime/projection.ts` |
| 会话连接与命令端口（`ChatConnection`/`ChatCommandPort`） | `npm-packages/peri-cf/web/runtime/connection.ts` |
| 新聊天 ACP 创建与命名（一次性宿主） | `npm-packages/peri-cf/web/runtime/creation.ts` |
| Turso 元数据查询与创建回读（relay URL + 浏览器配置的上游与 token） | `npm-packages/peri-cf/web/runtime/repository.ts` |
| ACP 初始化能力与工作区身份 | `npm-packages/peri-cf/web/runtime/bootstrap.ts` |
| 反向交互 live 登记表（权限/提问） | `npm-packages/peri-cf/web/runtime/interactions.ts` |
| 浏览器诊断与公开错误（不含凭据） | `npm-packages/peri-cf/web/runtime/logging.ts` |
| 共享展示 DTO（Entry/Block/Tool/Turn/Interaction/任务/计划/队列） | `npm-packages/peri-cf/shared/chat.ts` |
| SyncState 与运行元数据 Zod schema | `npm-packages/peri-cf/shared/sync.ts` |
| SDK view 到 UI 的展示物化（`materializeSyncState`；unsupported pair 由 `web/api/client.ts` 拒绝并提示升级） | `npm-packages/peri-cf/shared/sync-state.ts` |
| wasm 构建输入常量（内存/栈/清理超时） | `npm-packages/peri-cf/shared/runtime-limits.ts` |
| 浏览器 HTTP/本地只读查询门面（list/create/detail） | `npm-packages/peri-cf/web/api/client.ts` |
| 聊天展示、导航与生成生命周期 | `npm-packages/peri-cf/web/chat/`、`web/App.tsx` |
| TanStack 私有读侧缓存与取消 | `npm-packages/peri-cf/web/chat/queries.ts` |
| CF 专用构建、provenance 与原子准备（哈希范围 = peri-wasm 本地依赖闭包） | `npm-packages/peri-cf/scripts/{build-wasm,wasm-provenance,prepare-wasm}.ts` |
| 宿主定时器与 socket 胶水注入（`periSetTimeout`/`periClearTimeout`/`periSetImmediate` 经 scheduler 端口） | `npm-packages/peri-cf/scripts/{worker-glue,worker-runtime-glue}.ts` |
| 构建 profile 产物对比（不测 runtime CPU） | `npm-packages/peri-cf/scripts/profile-wasm.ts` |
| 本地 workerd 启动与日志转发 | `npm-packages/peri-cf/scripts/local-preview.ts`、`local-preview-command.ts`、`relay-output.ts` |
| 行为测试 | `npm-packages/peri-cf/tests/`、`web/**/*.test.tsx`（浏览器宿主与 relay 的行为验收以真实浏览器手工验证为准） |

WASM 产物来源及协议验证入口参见 [Peri WASM 索引](peri-wasm.md)，通用 SDK 契约参见 [TS SDK 索引](peri-ts-sdk.md)。

聊天展示对象契约见 [peri-cf 聊天对象契约](../design/peri-cf-chat-objects.md)：产品语义（消息展示、命令、交互回答、载荷读取）继续有效；**执行与同步结构已由 [浏览器宿主 Agent 设计](../design/peri-cf-browser-agent.md) 取代**（2026-10-11）：计算实例在浏览器，Worker 无状态透传，WS 同步与实例观测端点已删除。
