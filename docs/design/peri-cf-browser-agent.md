# Peri CF 浏览器宿主 Agent（历史设计）

> **已被取代（2026-10-21）**：执行位置与配置边界改由
> [peri-cf-server-agent.md](peri-cf-server-agent.md) 约束。本文件保留浏览器宿主时期的
> 生命周期、重放与命令语义作为历史依据；与现行设计冲突处以新文件为准。
>
> **历史符号（已删除）**：本文提到的 `ManagedAgents`、`AgentManager`、`SessionDocs`、
> `SessionViewStore`、`SyncState`、`/commands`、`/events`（SSE）与 Worker/DO 路径均不代表
> 当前实现。当前入口是 SDK `ChatManager`/`ChatService`/`ChatView`/`ChatReplica`、HTTP Action
> 与 WebSocket `/api/chats/:id/sync`；见 [Peri SDK Chat](peri-sdk-chat.md) 与
> [Peri CF 索引](../code-index/peri-cf.md)。

状态：已批准目标设计（2026-10-11）。本文件替代 [peri-cf-chat-objects.md](peri-cf-chat-objects.md)
中「Worker 承载 ACP 宿主、浏览器为只读副本、WS 同步」的执行与同步结构；产品语义（消息
展示、命令、交互回答、载荷读取、实例观测的可见范围）继续有效，冲突处以本文件为准。
2026-10-11 增补「配置与凭据」裁决（浏览器 BYOK、Worker 零配置），见下节。
迁移范围、顺序与验收见 [active issue](../../spec/issues/2026-10-11-peri-cf-browser-agent-migration.md)。

## 目标与边界

1. **浏览器是唯一计算实例**：Peri ACP Host（peri-wasm）在浏览器内启动；会话运行时、
   投影、命令处理、交互回答、载荷读取全部驻留前端。
2. **Worker 零配置无状态**：只提供静态资源与 relay 透传；不持有会话运行时、不保存同步
   状态、不做执行调度、不注册实例，也不持有上游地址、凭据与任何 env 变量。

## 数据流与依赖方向

| 方向 | 路径 |
| --- | --- |
| 模型 | 浏览器 Rust fetch transport → 同源 `/api/relay/model/<encodeURIComponent(上游基址)>/<上游路径>` → 该基址的模型端点（响应流式透传） |
| 存储 | 浏览器 Rust 驱动的 fetch executor → 同源 `/api/relay/storage/<encodeURIComponent(上游基址)>/<上游路径>` → 该基址的 libSQL HTTP 接口 |
| 配置 | 浏览器本地配置（设置页写入 localStorage 单键）；没有服务端配置面 |

约定：

- relay 按请求指定的目标转发：上游由路径第一段携带，Worker 不保存也不限制目标。
- 凭据由浏览器携带、原样上行；逐跳头与跳源头被剥离。
- 授权不由应用承担：入口控制（Cloudflare Access 或等效设施）是部署必要条件。

## 配置与凭据（2026-10-11 裁决）

1. **决策**：放弃「模型 key / 存储 token 不进浏览器、只在 Worker 注入」的旧约束，改为
   浏览器 BYOK 零配置后端。安全姿态由「密钥不出浏览器」改为「入口控制承担访问授权」。
2. **配置面**：`frontend/settings/model/config.ts` 用 localStorage 单键 `peri-cf.browser-config.v1` 保存
   `{ machineId, model: { provider, id, baseUrl, apiKey }, storage: { url, token } }`。
   `SettingsView` 兼作首次运行门禁（必填项不齐只渲染设置页，宿主不启动）与标题栏齿轮入口；
   配置在宿主启动时读取，保存后刷新页面生效，运行期不热切换凭据。machineId 由客户端
   `crypto.randomUUID()` 首次生成并长期保存，是 Rust Store 的机器身份（须为规范 UUID）；
   清除 localStorage 或换浏览器即换身份，旧会话归旧身份。
3. **relay 合同**：`/api/relay/<kind>/<encodeURIComponent(上游绝对基址)>/<上游路径>?query`，
   kind ∈ {storage, model}；基址必须为 `http(s)`，路径首段缺失、解码失败或协议不符返回 400。
   凭据头（`authorization` / `x-api-key` / `api-key`）原样上行，不再剥离或注入；逐跳头与跳源头
   （cookie、host、content-length、content-encoding、accept-encoding、connection、origin、
   referer、cf-*、x-forwarded-*）与上游响应中的编码/长度/连接/Set-Cookie/cf-* 字段被剥离。
   存储 JSON 响应的 `base_url` 改写为 relay 前缀；模型响应流式透传、不缓冲。不再提供
   `GET /api/config`。
4. **风险（已知，代码不做限制）**：relay 按请求目标转发且无 host 白名单，未挂入口控制时
   等价开放转发面；凭据存 localStorage 并经 relay 明文过网（DevTools 网络可见）；machineId
   由客户端自行生成，不是认证凭证。
5. **影响**：Worker env 与 secrets 全部移除（`backend/types.ts` 仅剩 `ASSETS`，`wrangler.jsonc`
   无 vars，`.dev.vars.example` 删除）；部署步骤只剩构建、部署与入口保护；旧会话与新身份
   机器归属不一致。
6. **状态**：实现已落地；用户手工验收未进行（进度与验收边界见迁移 issue）。

## 结构决策

1. **存储传输 seam**：`turso_serverless` 的驱动（peri-resources 的远程 Store 唯一入口）把网络
   原语抽成 `HttpExecutor`：默认 reqwest，Emscripten 目标经 `wasm-fetch` feature 改用宿主
   `fetch`（仓库补丁 `patches/turso-serverless-0.1.3-wasm-fetch.patch`）。浏览器构建因此仍由
   Rust 侧 Store 持有 schema、codec、ledger、错误分类与事务边界，只有 HTTP 原语换实现；不引入
   JS store 客户端，也不需要新的注入端口。CF/native 构建继续用 reqwest 后端，行为不变。
2. **模型传输**：复用 peri-model fetch transport（`cloudflare` feature 路径）；浏览器构建
   不需要 socket/DNS 层，sockets 相关胶水不得被调用。
3. **本地 Yjs**：SessionDocs 只在该浏览器内使用；不再跨网络同步。hello/ack/sync 帧与
   交付信用在应用内删除；SDK 公共导出保留，删除按 SDK 兼容义务另行评估。
4. **会话生命周期**：刷新/关闭标签页等价于宿主实例结束（与原 isolate 回收同语义）：
   在途执行丢失且不复原，展示态由下一次 `session/load` 重放重建。
5. **多标签页**：每标签页一个宿主实例；跨标签页一致性由远端 Store 承担，不做跨标签页
   协调、不迁移执行、不互相观察。
6. **工具**：沿用默认关闭；接入工具仍须按 MCP 能力边界单独设计授权与生命周期。
7. **删除清单**（不做兼容层）：迁移前的 `worker/chat/*`、`worker/wasm/*`、`frontend/shared/sync.ts` 的握手与
   控制帧、浏览器 SessionDocReplica/SessionDocSync 的传输用途、`/api/v2/*` 会话面与实例
   观测端点。

## Agent 管理模块（2026-10-20，历史；已删除）

> 本节描述的 `AgentManager` 及其 `frontend/agent/` 边界已在 SDK Chat 重构中删除，不得作为
> 当前入口。现行页面控制器是 `frontend/remote/controller.ts`，服务端控制面是 SDK
> `ChatManager`/`ChatService`。

为后续后端迁移，浏览器内的会话管理逻辑已收敛到平台无关的 `frontend/agent/`：

1. `AgentManager` 是唯一门面：`listChats` / `createChat` / `readChat` / `openChat` / `send` / `stop` / `answerInteraction` / `readPayload` / `subscribe` / `dispose`。视图层只订阅只读快照，不再持有会话状态机。
2. 平台差异经 `AgentEnvironment` 端口注入：`cwd`、系统指令、ACP `initialize`、MCP 装配/冻结/就绪与诊断日志；宿主启动与仓储分别是 `StartHost` / `ChatRepository`。
3. 浏览器适配留在 `frontend/runtime/`：`application.ts` 是组合根，`environment.ts` 注入 localStorage 配置与本地 ACP MCP，`repository.ts` 经同源 relay 读 Turso。`frontend/agent/` 不依赖 React、DOM、localStorage 或 TanStack Query，边界由 `frontend/agent/boundary.test.ts` 守住。
4. 模块组织：文件夹即模块边界，目录根 `index.ts` 是唯一出口。`frontend/agent/` 内分 `session/`（运行期、注册表、创建）、`view/`（投影、交互、问答）与 `transport/`（初始化、命令通道）；`frontend/` 内按 `app/`、`chat/{runtime,components}/`、`runtime/`、`host/`、`settings/{model,components}/`、`mcp/`、`local-fs/` 划分。边界测试同时禁止跨模块深引用。
5. 本轮只做逻辑分层与端口化，不引入 HTTP/Worker 传输层；后端实例化时替换上述端口即可复用门面与运行期。

## 风险与未决

- Emscripten/Tokio 在浏览器事件循环中运行宿主是首个验证点；若不成立，本设计需重新裁决。
- wasm 产物约 11.8 MB（gzip 约 5 MB）；首屏成本与缓存策略另行评估。
- relay 按请求目标转发且不限制 host，未挂入口控制时等价开放转发面：部署侧入口控制是
  必要条件，不是可选项；凭据在浏览器与网络中明文可见（见「配置与凭据」）。
- 第三方补丁随 crate 版本升级必须复核：`fetch` executor 与驱动上游的 `session.rs` 修改面
  绑定，升级 `turso_serverless` 时按 `patches/README.md` 重新生成补丁与校验值。
