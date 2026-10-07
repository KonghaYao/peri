# Peri CF

## Scope

独立的 React/Vite + Cloudflare Workers 聊天应用，不属于根 Cargo workspace。按需读取 `../../docs/standards/index.md`，跨层与安全变更读取 architecture-contracts，测试读取 testing，文档读取 documentation。

## 边界

浏览器经同源 Hono API 调用每聊天的 Durable Object；API 与 DO 路由共享鉴权和错误 middleware。`worker/api` 负责 HTTP，`chat` 负责会话生命周期，`execution` 负责准入与 DO 存储适配，`wasm` 负责实例化与配置；前端按 `web/api`、`web/chat`、`web/settings` 分组。

服务端通过既有 `@peri-code/sdk/wasm` 和静态 Emscripten 产物连接 ACP Host。工作区 SDK 内部模块只在 `worker/sdk/index.ts` 集中接线；不新增 SDK public entry、改构建脚本或在应用复制准入规则。SDK 仅保留 `admission-core.ts` 与 SQLite `admission-service.ts` 的隔离，DO 注入 async durable registry/ledger，不导入 Bun/stdio 主入口。当前应用依赖相邻 SDK 源码目录；独立发布需重新评估正式入口，而不是偷偷扩大 SDK API。

`GET /api/chats/:id/resources` 在应用层观测每次 WASM Host 启动的独立身份、实际 SDK 代次与导出线性内存；不把 Worker 进程指标归属于 Agent。没有实例 CPU 计数时明确返回不可用，关闭后只保留最后观测，DO 重启不恢复虚假的存活实例。契约与限制见 README 的 Agent 实例资源查询。

列表由 TS 经 SDK `TursoStorage` 直接查询外部 Turso/libSQL 的 Rust Store，不设置目录 DO，不复制聊天列表或维护平行会话表。新聊天经 ACP 创建和命名，再从 Store 查询读回；聊天 ID 就是实际 ACP Session ID。每聊天 DO 保存展示历史与执行协调状态，DO 与 Rust Store 没有跨存储事务，不依赖 D1。共享 Bearer token 是一个信任域，不是多租户身份。模型与存储密钥禁止进入浏览器或 `VITE_*` 环境变量。

同聊天只允许单轮执行。取消必须等待真实 ACP Host 关闭及状态持久化，未确认停止应阻断重跑。实例重启不自动恢复在途执行；展示消息不得重造 Rust 工作义务。

`shared/chat.ts` 的 Zod schema 是前后端 DTO 的单一来源。HTTP 边界复用 Hono 鉴权、按实际字节限制请求体和 validator。ACP 投影复用 SDK `SessionDocs`；WebSocket 同步复用 `SessionDocSync` 与 `SessionDocReplica`，不自写文本累加、序号或状态向量同步规则。展示通过 SDK view 读取，Yjs app metadata 不重复保存完整文本；浏览器副本只读，发送/取消仍走命令边界。WS 首帧鉴权前不发送文档，令牌不进入 URL；断线仅恢复订阅，不自动取消或重新执行。

TanStack Query 只管理读侧查询；每认证工作区持有独立客户端，token 切换须卸载旧工作区并关闭旧连接、取消查询、清空缓存。禁用自动查询重试/重取，不把发送或停止接入自动重放；执行与取消领域状态机保持显式。普通 WebSocket 不等于实现了 Hibernation；在途执行恢复仍未完成。

仅显式新消息允许恢复本应用 own-stop 留下的暂停，必须匹配持久化控制代次，不覆盖外部控制权。`PERI_MACHINE_ID` 是部署的持久 UUID，重启时不得重新生成。

## 路由与命令

入口及部署条件见 `README.md` 与 `../../docs/code-index/peri-cf.md`。

- `bun run test`：本包行为与展示测试。
- `bun run dev`：构建后在本地 workerd 启动真实 Worker，默认 8791；Vite HMR 单独用 `dev:hmr`，其外部存储 I/O 路径不作为完整 WASM 验收入口。
- `bun run typecheck`、`bun run build`：TS 与完整 CF/前端构建。
- `SQLD_BIN=/path/to/sqld bun run smoke:local`：真实本地 workerd/WASM、模型 fixture 和 sqld 闭环。
- `build:wasm` 通过仓库脚本构建 CF 专用 feature/profile 及 provenance；`prepare:wasm` 只校验并原子发布匹配当前源码的应用缓存，不复用 SDK 默认二进制，不在轻量准备中隐式编译 Rust。

生产部署需要服务端 secrets 和实际套餐资源验收。默认不接本地文件工具；远程能力需经 MCP 单独设计授权。
