# Peri CF

## Scope

独立的 React/Vite + Cloudflare Workers 聊天应用，不属于根 Cargo workspace。按需读取 `../../docs/standards/index.md`，跨层与安全变更读取 architecture-contracts，测试读取 testing，文档读取 documentation。目标设计见 `docs/design/peri-cf-browser-agent.md`。

## 边界

**浏览器是唯一计算实例**：Peri ACP Host（peri-wasm）在浏览器内启动，会话运行时、投影、命令处理、交互回答与载荷读取全部驻留前端。**Worker 零配置无状态**：只提供静态资源与 relay 透传，不持有会话运行时、不保存同步状态、不做执行调度、不注册实例；没有 env/secrets、没有 Durable Object、没有数据库表、没有展示持久化。

浏览器不经网络直连上游：模型与存储流量都走同源 relay，上游目标由请求路径第一段携带（`/api/relay/<storage|model>/<encodeURIComponent(上游绝对基址)>/<上游路径>`，基址必须 `http(s)`，否则 400）。relay 不持上游地址与凭据：客户端凭据头（`authorization` / `x-api-key` / `api-key`）原样上行，逐跳头与跳源头被剥离；存储 JSON 响应的 `base_url` 改写为 relay 前缀（baton 亲和不由浏览器直连）。**凭据在浏览器**：模型 API Key 与存储 token 由设置页填写、存于 localStorage（单键 `peri-cf.browser-config.v1`），经 relay 明文过网，DevTools 网络可见（2026-10-11 用户裁决，取代旧「密钥不出浏览器」约束）。

**本应用没有访问控制**（无令牌、无身份验证）；relay 按请求目标转发且不限制 host，未挂入口控制时等价开放转发面（已知风险，代码不做限制），部署入口控制由 Cloudflare Access 或等效设施承担，是必要条件而非可选项。machineId 由客户端 `crypto.randomUUID()` 首次生成后长期保存（必须是规范 UUID），写入 Rust Store 的机器身份；清除 localStorage 或换浏览器即换身份，旧会话归旧身份。

会话清单与创建由浏览器 TS 经 SDK `TursoStorage` 查询 Rust Store（`/workspace` 下会话）；新聊天经一次性宿主的 ACP `session/new` + `session/rename` 创建，再查回。**唯一持久权威是 Turso/libSQL 的 Rust Store**；Yjs 展示态与运行元数据只存在于该标签页。

## 数据流与生命周期

- 宿主按需启动：每次执行/复原启动一个 ACP Host，用完关闭；实例边界提供关闭证据，未确认关闭会阻断该会话后续运行。
- 复原纪律：**先建 runtime/订阅，再触发 `session/load`**，不得丢弃重放通知；`GET`/`read()` 返回与执行期同一 SyncState DTO。
- 刷新/关闭标签页等价于宿主实例结束：在途执行丢失且不复原，展示态由下一次 `session/load` 重放重建。多标签页各自独立，不互相观察、不迁移执行。
- 同会话只允许单轮执行。取消走会话级 `session/cancel` 通知，没有执行准入或精确停止目标：未确认停止必须阻断重跑。命令经内存调用（不再有 WS 控制帧）；失败总是「确定未生效」，不存在需要阻断重跑的未确认中间态。
- 浏览器构建不装配 sockets/DNS：只注入调度器端口；`crypto`/`node:crypto` 以显式失败的替身替换（`web/shims/nodeCrypto.ts`，SDK `/portable` 的准入面不被消费）。
- 配置在宿主启动时读取（`web/host/config.ts` ← `web/settings/config.ts`）：必填项不齐时只渲染设置页、宿主不启动；保存后由页面刷新让新配置生效，运行期不热切换凭据。

## SDK 消费边界

只消费已发布 SDK 入口：`@peri-code/sdk/wasm`（平台无关的 Emscripten 宿主启动、调度器端口与关闭证据）、`/portable`（平台无关语义：投影、Store 读取、交互规则）、`/view`（只读视图与载荷读取）。不引用 `@peri-sdk/src`、不新增 SDK entry、不在应用复制准入或同步规则。SDK 宿主启动顺序、回调所有权与清理证据归 SDK；应用只经 `moduleFactory` / `ports` / `onLifecycle` 注入平台适配。

`shared/chat.ts` 与 `shared/sync.ts` 的 Zod schema 是展示 DTO 的单一来源（客户端投影读取形态）。ACP 投影复用 SDK `SessionDocs`，展示经 SDK view 读取；不复制文本累加、序号或状态向量规则。

TanStack Query 只管理读侧查询；每个会话工作区持有独立客户端，切换会话须卸载旧工作区、取消查询、清空缓存。禁用自动查询重试/重取，不把发送或停止接入自动重放。

## 路由与命令

入口及部署条件见 `README.md` 与 `../../docs/code-index/peri-cf.md`。

- `bun run test`：本包行为与展示测试。
- `bun run dev`：构建后在本地 workerd 启动真实 Worker，默认 8791。
- `bun run dev:hmr`：Vite HMR 开发（宿主 wasm 产物仍需先 `prepare:wasm`）。
- `bun run typecheck`、`bun run build`：TS 与完整 CF/前端构建。
- `build:wasm` 通过仓库脚本构建 CF 专用 feature/profile 及 provenance；`prepare:wasm` 只校验并原子发布匹配当前源码的浏览器缓存（`web/wasm/generated`），不在轻量准备中隐式编译 Rust。`shared/runtime-limits.ts` 属于 wasm 构建输入，改动会导致缓存失效并需重建。

默认不接本地文件工具；远程能力需经 MCP 单独设计授权。工具环境与部署端资源（免费档 CPU、长时执行生命期）尚未验收。
