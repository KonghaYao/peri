# Peri CF

React + Vite 聊天前端 + 零配置无状态 Cloudflare Worker。**浏览器是唯一计算实例**：Peri ACP Host（peri-wasm）在浏览器内启动，会话运行时、投影、命令处理、交互回答与载荷读取全部驻留标签页；Worker 只提供静态资源与 relay 透传，不持有会话运行时、不保存同步状态、不做执行调度、不读取任何 env 变量。跨标签页与跨刷新唯一的持久权威是 Turso/libSQL 的 Rust Store（经同源 relay 访问），展示态由 ACP `session/load` 的历史重放重建。

这是待部署验收的单可信用户示例，不是已完成生产验证的服务。真实浏览器内的 Emscripten/Tokio 宿主、平台资源预算与远端 Store 均未验收；构建成功不等于 runtime 可用。

## 配置与信任边界

- **浏览器 BYOK，配置不进服务端**：模型 Provider/ID/Base URL/API Key、存储 URL/token 与 machineId 在设置页（`web/settings/SettingsView.tsx`）填写，以单条 JSON 保存在本机 localStorage（键 `peri-cf.browser-config.v1`）。请求按真实值经同源 relay 上行；Worker 不保存、不注入、也不读取任何配置项。
- **密钥在浏览器侧可见**（2026-10-11 用户裁决，取代旧约束「模型 key / 存储 token 不进浏览器、只在 Worker 注入」）：凭据存于 localStorage，并经 relay 明文过网，浏览器 DevTools 网络面板可见；授权由部署入口控制承担，不由应用承担。
- **relay 按请求目标转发**：`/api/relay/<kind>/<encodeURIComponent(上游绝对基址)>/<上游路径>?query`，kind 为 `storage` 或 `model`，基址必须是 `http(s)`（否则 400）。Worker 不持有上游地址与凭据，也不限制目标 host——未挂入口控制时等价于开放转发面（已知风险，代码不做限制，见下）。
- **头处理**：客户端凭据头（`authorization`、`x-api-key`、`api-key`）原样上行；逐跳头与跳源头（`cookie`、`host`、`content-length`、`content-encoding`、`accept-encoding`、`connection`、`origin`、`referer`、`cf-*`、`x-forwarded-*`）剥离；上游响应剥离 `content-encoding`、`content-length`、`transfer-encoding`、`connection`、`set-cookie`、`cf-*`。响应流式透传（模型 SSE 不缓冲），存储 JSON 响应的 `base_url` 改写回同一 relay 前缀（baton 亲和不由浏览器直连上游）。
- **没有任何访问控制**：`/api/*` 与页面本身对拿到地址的任何人开放。部署入口控制（Cloudflare Access 或等效设施）是必要条件，不是可选项。生产环境不要直接暴露未受保护的域名。
- **machineId 是客户端身份**：首次运行由浏览器 `crypto.randomUUID()` 生成并长期保存，作为写入 Rust Store 的机器身份（必须是规范 UUID）；清除 localStorage 或换浏览器即换新身份，旧会话仍归属旧 machineId。它不是认证凭证。
- 会话清单与创建由浏览器 TS 经 SDK `TursoStorage` 查询 Rust Store（`/workspace` 下会话）；`Chat.id` 就是 ACP Session ID，不另设身份，也不维护平行会话表；查询失败不当作空列表。

## 浏览器宿主的生命周期

- 宿主按需启动：每次执行/复原启动一个 ACP Host，用完关闭。启动、关闭的排序与「已确认清理」证据由 SDK `startPeriWasmHost` 提供；应用只经 `moduleFactory` / `ports` / `onLifecycle` 注入平台适配，并经 `AbortSignal` 与截止时间控制启动。
- 复原纪律：**先建 runtime/订阅，再触发 `session/load`**，不得丢弃重放通知；读取返回与执行期同一 SyncState DTO。
- 刷新/关闭标签页等价于宿主实例结束：在途执行丢失且不复原，展示态由下一次 `session/load` 重建。多标签页各自独立，不互相观察、不迁移执行。
- 启动失败或关闭未确认时，该标签页内该会话被标记为禁止继续执行（历史仍可读）；标记只在内存，随页面结束消失。不要通过重启强行认领未知是否停止的执行。
- 同会话只允许单轮执行；单轮助手输出按 JSON 编码后文本计算限制 512 KiB（`MAX_REPLY_BYTES`），超限取消执行并保留已接受的部分回复、以 `error` 终结。

## 命令与取消语义

命令不再经网络：`web/runtime/session.ts` 的本地命令（prompt / cancel / interaction / payload）在内存中调用宿主，失败总是「确定未生效」，不存在需要阻断重跑的未确认中间态。失败码保持稳定字符串：prompt 为 `busy` / `blocked`，cancel 为 `cancel_timeout` / `cancel_failed`，interaction 为 `settled` / `invalid_answer`，payload 为 `unavailable` / `integrity_error` / `too_large`，其余为 `not_found` / `internal_error`。

取消走会话级 `session/cancel` 通知：当前 ACP 没有执行准入、精确停止目标或控制代次，取消成立以 prompt 结算为 `cancelled` 且宿主关闭被确认为准；未确认的关闭阻断该会话后续运行。启动中尚未发送 prompt 时只跳过本轮，不发送通知。取消后的下一次显式发送是新回合，没有恢复握手。

## 入口与 API

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| ALL | `/api/relay/storage/<编码基址>/<上游路径>` | 转发到路径首段解码出的上游（libSQL HTTP 接口）；凭据按客户端真实值上行；JSON 响应的 `base_url` 改写为 relay 前缀 |
| ALL | `/api/relay/model/<编码基址>/<上游路径>` | 转发到路径首段解码出的上游（模型端点，响应流式透传）；凭据按客户端真实值上行 |

没有 `GET /api/config`（已删除）：Worker 不返回任何配置，配置只存在于浏览器。relay 路径首段缺失、URL 解码失败或不是 `http(s)` 时返回 400；其余 `/api/*` 返回 404 JSON 且 `Cache-Control: no-store`；非 API 路径交给静态资源（SPA 回退）。会话列表、创建、读取与命令没有 HTTP 端点——全部发生在浏览器内。

浏览器宿主的运行配置（`web/host/config.ts`）从浏览器本地配置取上游基址与凭据，把 `storage.url` 与 provider `baseUrl` 编码进同源 relay 路径（`web/settings/config.ts` 负责 localStorage 读写），`cwd` 固定 `/workspace`，`meta_harness` 直接使用 SDK `BareHarnessConfig`（默认关闭工具段落）。

## 本地启动与构建

需要 Bun；构建 WASM 还需要 Rust `wasm32-unknown-emscripten` target 与 Emscripten 工具链（见仓库 `scripts/cargo-wasm.sh`）。本地 Store 需要**独占**的 sqld（libSQL HTTP）实例：peri 的 canonical 表名（`projects` / `messages` / `threads` 等）与别的应用共用一个库会撞名，非本构建代数的旧库也会被 fail-closed 拒绝（拒绝发生在只读的身份读取阶段，宿主启动报 `behavior is unsupported here (NotApplied)`）。

```bash
sqld --no-welcome --http-listen-addr 127.0.0.1:8090 --db-path /tmp/peri-cf-dev-store
```

存储与模型配置**不在仓库文件里**：首次打开页面时未配置则只渲染设置页（宿主不启动），在该页填写存储 URL（须 `http(s)`，本地 sqld 如 `http://127.0.0.1:8090`）、存储 Token、模型 Provider/ID/Base URL/API Key；保存后刷新页面即对宿主生效。配置只写入该浏览器的 localStorage；本机 `.dev.vars` 里保留的既有取值可供手工抄录参考，但应用已不读取它（`.dev.vars.example` 已删除，Worker 侧无 env 变量）。空库首轮打开会建立 store 身份与 schema。

```bash
bun run build:wasm      # 构建 CF feature/profile 的 WASM 并写入 provenance（约 2–3 分钟）
bun run prepare:wasm    # 校验并原子发布到 web/wasm/generated
bun run dev             # 构建后在本地 workerd 启动真实 Worker（默认 8791，日志写入 logs/）
bun run dev:hmr         # 仅前端 HMR（宿主 wasm 产物仍需 prepare:wasm）
bun run typecheck
bun run build           # prepare:wasm + vite build（客户端与 Worker 一起打包）
bun scripts/local-preview.ts  # 重启本地预览（等价 bun run preview），读取已构建的 dist/peri_cf/wrangler.json
```

改配置不必重启 Worker：在设置页保存后页面自行刷新。改了前端或 Worker 代码才需要 `bun run build` 后重启本地预览。

WASM 产物必须与当前源码一致：`prepare:wasm` 验证源码、锁文件、工具链、构建参数与产物哈希后原子发布；缺失、过期或混合产物直接失败，不会把旧二进制标记成当前源码。纳入哈希的 Rust 源码是 `peri-wasm` 及其本地（path）依赖闭包；`Cargo.toml`、`Cargo.lock`、`patches/`、`.cargo/`、仓库脚本、`npm-packages/peri-cf/scripts/{build-wasm,wasm-provenance,wasm-profile,wasm-tools,worker-glue,worker-runtime-glue}.ts` 与 `shared/runtime-limits.ts` 按文件哈希——改动这些输入后必须重新 `build:wasm`。`cf-wasm-size`（默认）与 `cf-wasm-speed` 可用 `bun run build:wasm --profile <name>` 构建，`bun run wasm:profiles` 对比体积与构建耗时。

## SDK 消费边界与平台适配

只消费已发布 SDK 入口，不引用 `@peri-sdk/src`、不新增 entry、不在应用复制准入或同步规则：

| 入口 | 用途 |
| --- | --- |
| `@peri-code/sdk/wasm` | 平台无关的 Emscripten 宿主启动、调度器端口、ACP transport、截止时间与关闭证据 |
| `@peri-code/sdk/portable` | 平台无关语义：ACP 初始化、`SessionDocs` 投影、`TursoStorage`、交互规则 |
| `@peri-code/sdk/view` | 只读视图与载荷读取 |

浏览器构建不装配 sockets/DNS：只注入调度器端口（`createSchedulerPort`）。SDK `/portable` 重导出的准入模块引用 `crypto`，浏览器侧由 `web/shims/nodeCrypto.ts`（Vite alias）替换为**显式失败**的替身——peri-cf 不消费准入面，一旦被调用说明消费面变化，需要重新裁决。

| 保留位置 | 为什么是宿主适配而不是自建底层实现 | 移除条件 |
| --- | --- | --- |
| `web/host/` | 浏览器模块加载（Vite 资源 URL + 显式 `instantiateWasm`）与同源 relay 路径装配 | SDK 提供浏览器资源加载适配时 |
| `web/runtime/` | 标签页内的会话运行期、投影、命令与关闭观察；生命周期建立在 SDK 宿主与清理证据之上 | SDK 提供浏览器端会话运行期时 |
| `web/settings/` | 浏览器本地配置（localStorage 单键）与设置页门禁属本应用产品边界 | 不适用 |
| `worker/api/`、`worker/index.ts` | relay 透传与静态资源是本应用产品边界 | 不适用 |
| `scripts/`、`vite.config.ts` | WASM provenance、宿主定时器 glue 注入（`periSetTimeout`/`periClearTimeout` 经 scheduler 端口）与 Wrangler 属构建与验证工具 | 不适用 |

本包与 SDK 均未定义 `lint` script；验证入口为各自的 `typecheck`、`test`、`build`。

## 目录职责

- `shared/`：展示 DTO 的 Zod schema（`chat.ts` 对象契约、`sync.ts` SyncState 与运行元数据）与 wasm 构建输入常量。
- `worker/api/`：Hono app、relay 透传与 HTTP 边界；`worker/index.ts` 是唯一入口，`worker/types.ts` 的 Env 只有 `ASSETS` 绑定。
- `web/host/`：Emscripten 模块工厂与浏览器宿主启动（`startBrowserHost`）。
- `web/settings/`：浏览器本地配置读写（`config.ts`，localStorage 单键 `peri-cf.browser-config.v1`）与设置页（`SettingsView.tsx`：首次运行门禁 + 标题栏齿轮入口）。
- `web/runtime/`：标签页应用装配（application/registry/repository）、会话运行期（session）、投影（projection）、命令连接（connection）、交互与创建。
- `web/chat/`：TanStack Query 读侧、会话视图状态机、消息展示与面板。
- `web/shims/`：浏览器构建的打包替身（见 SDK 消费边界）。
- `scripts/`：WASM 构建与 provenance、宿主定时器 glue、本地预览与日志转发。

## 库与自有逻辑

- Hono 承载 Worker 侧 HTTP；`apiApp()` 统一 `Cache-Control: no-store`、HTTPException 错误响应与 404 JSON。
- Zod 是展示 DTO 的单一 schema 来源；浏览器校验投影物化结果，不以 TypeScript 静态类型代替外部输入检查。
- ACP → Yjs 投影复用 SDK `SessionDocs`，展示经 SDK view 读取；不复制文本累加、序号或状态向量规则。展示态与运行元数据只在该标签页内存，不写回持久层。
- TanStack Query 只管理读侧查询；切换会话卸载旧工作区、取消查询并清空缓存，不把发送或停止接入自动重放。
- 模型与 Rust Store 的 HTTP 由 Rust 侧 transport（Emscripten `fetch` 执行器）发起，经同源 relay；浏览器不直连上游。

## WASM 限制与验证范围

WASM 运行的是 ACP Host，不是本地终端工具环境：没有宿主项目文件系统、任意 shell/子进程或本地 builtin MCP 服务。默认关闭工具能力；若接入远程 MCP，需独立设计授权、环境生命周期与网络可达性。

每个 CF WASM 线性内存最大 64 MiB、栈 8 MiB、增长步长 1 MiB（`shared/runtime-limits.ts` 是构建输入）。CF feature 的模型 HTTP/SSE 走 Emscripten `fetch` 执行器；`turso_serverless` 的 HTTP 原语经仓库补丁（`patches/turso-serverless-0.1.3-wasm-fetch.patch`，`wasm-fetch` feature）在同一执行器上运行——补丁与 `scripts/cargo-rmcp-patched.sh` 的校验值绑定，升级 crate 时必须按 `patches/README.md` 重新生成。命令型 hook 在 Emscripten 下明确报错，不静默放行。

Fetch adapter 接受当前提供商使用的缓冲 JSON 请求体；响应队列最多两块 64 KiB，原始读取块超过 1 MiB 明确拒绝。取消、调用 future 或 body 丢弃时中断网络并释放 reader；重定向使用 `manual`，不跨域自动转发凭据。传输契约可在仓库根复验：

```bash
EMSDK_PYTHON=$(command -v python3) ./scripts/cargo-wasm.sh build --locked --release --target wasm32-unknown-emscripten -p peri-model --features cloudflare --example cloudflare-contract
node scripts/test-cloudflare-fetch.mjs
```

该 runner 使用真实 Emscripten 模块与受控 Fetch fixture，验证状态/请求头/流、错误 cause、取消、丢弃、超时与队列边界，不调用付费模型，也不代替真实浏览器与远端 Store 验收。

## 远端部署

1. 登录 Cloudflare（`bunx wrangler login`），核对部署账号与环境。
2. **无需也不得配置环境变量、secrets、Durable Object 绑定或 migrations**：`wrangler.jsonc` 只有 Worker、静态资源与 `ASSETS` 绑定；上游地址与凭据全部由浏览器携带，Worker 零配置。
3. `bun run deploy`（= `bun run build` + `wrangler deploy`）。
4. 入口配置 Cloudflare Access 或等效保护，并验证其保护范围与身份策略：relay 可按请求路径指定的任意 `http(s)` 目标转发，未受保护的域名等于开放转发面。
5. 部署后由使用者在浏览器设置页填写存储与模型配置；配置只存在于该浏览器，不随部署分发。

本轮没有任何部署端验收：免费档 CPU、wasm 首屏加载（产物约 11.8 MB，gzip 约 5 MB）与长时执行生命期均未验证；远程 Turso 地址与令牌未提供。上线前必须实际验证列表/创建、连续聊天、取消、失败状态、刷新后重放复原与多标签页行为，并确认入口访问控制有效。运行诊断保留原始错误及 cause 与请求路径；部署端须控制日志访问权限。

## 测试

`bun run test`（`bun test tests web`）在本包执行：

- `tests/chat-queries.test.ts`：私有 QueryClient、缓存隔离、读请求取消、强制历史刷新与禁用自动重试。
- `tests/wasm-provenance.test.ts`、`tests/wasm-profile.test.ts`：WASM 构建输入的哈希范围、原子发布与 profile 契约。
- `tests/worker-glue.test.ts`、`tests/worker-runtime-glue.test.ts`、`tests/relay-output.test.ts`、`tests/local-preview-command.test.ts`：构建/开发脚本的 glue 与输出转发契约。
- `web/chat/messageStatus.test.tsx`、`web/chat/interaction-panel.test.tsx`：消息与交互面板的渲染行为。

Worker 侧执行面测试随 `worker/chat/*`、`worker/wasm/*` 一并删除；浏览器宿主、relay 与 wasm 运行时的行为验收目前以真实浏览器手工验证为准（见 `spec/issues/2026-10-11-peri-cf-browser-agent-migration.md`）。测试不启动真实 WASM、不调用远端模型或 Turso。
