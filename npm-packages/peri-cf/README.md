# Peri CF

React + Vite 聊天前端，Hono / Cloudflare Workers API，TypeScript 通过 SDK `TursoStorage` 直接读取 Rust Store 的真实会话列表与元数据，每个聊天由独立的 SQLite Durable Object 管理执行与展示状态。后端通过 `@peri-code/sdk/wasm` 接入 Peri Rust ACP Host，复用 SDK `SessionDocs` 的 Yjs 投影，经 `SessionDocSync` / `SessionDocReplica` 和 WebSocket 同步浏览器；模型调用与 Rust 会话存储仍由 Peri 执行。应用不依赖 D1，也没有聊天目录 DO。

这是待部署验收的单可信用户示例，不是已完成生产验证的服务。测试中的模拟执行器不证明真实模型、远端 Turso 或 Cloudflare 上线可用。

## 数据与信任边界

- `TursoChatRepository`：通过应用本地 `worker/sdk/index.ts` facade 引用 SDK 现有 `TursoStorage`，查询实际 `threads`。列表读取 `/workspace` 下的会话，ID、标题与更新时间来自 Rust Store，不另建目录索引或双写元数据。
- `CHAT_SESSIONS`（Durable Objects）：按实际 ACP Session UUID 隔离消息展示与执行状态；`Chat.id` 就是 ACP Session ID，不另设 `periSessionId`。同一聊天不允许并发生成。首次读取/发送从 repository 获取会话元数据，执行只加载该实际 ID，不在聊天 DO 内创建另一个 Rust 会话；GET 每次刷新权威元数据，已有聊天的取消不依赖数据库读取。
- 执行准入已接入平台无关的 SDK AdmissionCore 与聊天 DO 内的 durable execution registry/ledger，负责执行 ticket/admission，而不是用 `session/prompt` 绕过 SDK 准入。账本与 Rust Store 是不同持久化边界，没有跨存储原子事务；执行准入规则仍由 SDK 维护，Worker adapter 负责 DO 存储与 Host 生命周期接线。
- `PERI_STORAGE_URL` / `PERI_STORAGE_TOKEN`：外部 Turso/libSQL，保存 Rust Store 的会话、消息及执行数据。TS 元数据查询与 Rust Host 使用同一 Store；聊天 DO 的展示消息和执行账本不替代 Rust Store。
- `PERI_MACHINE_ID`：必须显式配置的稳定部署 UUID，用于 Rust Store 的机器身份；缺失或格式无效时拒绝启动，不再使用硬编码 fixture 身份。
- `MODEL_BASE_URL` / `MODEL_API_KEY` / `MODEL_ID`：服务端模型配置，不能使用 `VITE_*` 暴露密钥。
- 所有 `/api/*` 请求必须携带 `Authorization: Bearer <APP_AUTH_TOKEN>`。未配置 token 时拒绝访问；静态页面不等于受保护的 API。

`APP_AUTH_TOKEN` 只代表一个共享信任域，不是多用户身份系统：知道 token 的人可访问域内全部聊天，没有用户级所有权、租户隔离或细粒度授权。浏览器使用该 token，因此应只交给可信用户，避免日志、URL、截图或不可信脚本泄露。生产入口推荐加 Cloudflare Access；当前 Bearer 检查不能代替 Access 身份验证、限流、审计或完整的多租户设计。

## Agent 实例资源查询

`GET /api/chats/:id/resources` 使用相同的 Bearer 鉴权，响应禁止缓存。`sessionId` 是实际 ACP 会话身份；`instance` 是此 DO 当前或最近一次启动的 WASM Host，不是聊天列表或整个 Worker 的资源占用。读取已有 DO 会话不刷新 Turso，也不启动新的 Host。未运行过或 DO 重启后，`instance` 为 `null`，不伪造上一实例仍然存活。

- `instance.instanceId` 区分每次 Host 启动；`generationId` 在 transport 就绪后来自实际 SDK 代次。
- `phase` 区分加载、启动、就绪、关闭中、已关闭、启动失败与关闭未确认；`running` 表示聊天任务状态，`executionBlocked` 表示停止未确认导致的准入阻塞。
- `startup` 记录宿主侧测得的模块加载、原生启动与 ACP 就绪耗时（相对本次启动的单调计时）；未发生的阶段为 `null`，不推算也不补零。`endedAt` 只在观测到关闭、启动失败或关闭未确认时写入，保留首次终止时刻。
- `memory.allocatedBytes` / `pages` 直接采样该 WASM 实例导出的线性内存（每页 64 KiB）；不是 Rust 活跃堆、整个 isolate 的 RSS 或 Cloudflare 内存配额。`heapUsedBytes: null` 表示目前没有分配器使用量指标；`peakObservedBytes` 仅为已采样最大值，不保证捕获所有瞬时峰值。
- 内存未取得时为 `null`。实例关闭或启动失败后释放监测器的内存引用，保留 `observation: "last-observed"` 和 `observedAt` 的最后观测，不把历史值称为当前存活内存，也不保证平台立即 GC。
- `cpu.supported: false`，`timeMs` / `utilizationPercent` 为 `null`。Cloudflare 不提供此 WASM 实例的进程 CPU 计数；生产 Workers 的计时器仅在 I/O 后推进，不能把 prompt 耗时、I/O 等待或本地计时器差值冒充 CPU 时间。平台请求级 CPU 统计也不能归属为单个 WASM 实例。限制依据为 Cloudflare 官方 Performance and timers 文档：`https://developers.cloudflare.com/workers/runtime-apis/performance/`。若后续需要可用的实例 CPU 数据，应另设计具备计数能力的运行环境或平台遥测归因，不新增虚假估算。

该接口仅观测应用实例，不修改 SDK、不加载数据库列表、不暴露模型或存储配置。相关契约位于 `shared/resources.ts`，采样实现位于 `worker/wasm/resources.ts`。

## 实例监控页面

`GET /api/instances` 用同一 Bearer 鉴权按会话汇总上述观测，供前端“实例监控”页面（顶栏 Activity 按钮，路由 `#instances`）轮询展示实例身份、阶段、启动耗时、线性内存与挂载会话。

- 聚合只读取各聊天 DO 的内存采样器：从 Store 取会话列表后按固定并发上限逐会话请求 `GET /api/chats/:id/resources`，不新增存储、不写库、不跨会话合并身份。
- 单个会话读取失败（宿主不可达或响应不符合 schema）降级为该项 `failure`，不影响其他会话，也不把失败伪装成“没有实例”。“无观测”与“读取失败”在数据上区分。
- 观测仍然只存在于各聊天 DO 的内存中：DO 被回收后该会话不再有实例观测，页面不伪造存活实例，也没有历史记录。轮询会唤醒被观测的聊天 DO，这是页面刷新的已知代价；页面在后台标签页停止轮询。
- 页面只做只读展示，不提供启动、停止或清理实例的操作；会话操作仍走聊天本身。

## 本地启动

在本目录执行命令。需要 Bun；首次构建 SDK 还需要 Rust `wasm32-unknown-emscripten` target 与 Emscripten 工具链，参见 [SDK README](../@peri-sdk/README.md) 及仓库 `scripts/cargo-wasm.sh`。WASM 产物必须与当前 SDK/Rust 协议一致，不应以陈旧产物代替重建。

保留 SDK 既有 `dist/wasm.js` 公共 transport 入口；CF 二进制单独执行 `bun run build:wasm` 构建，不复用 SDK 的默认 WASM 二进制。该命令通过仓库 `scripts/cargo-wasm.sh` 构建 `peri-wasm --features cloudflare`，生成应用私有 `.cache/wasm/` 产物及 provenance。`prepare:wasm` 验证源码、锁文件、工具链、构建参数及产物哈希后原子发布；缺失、过期或混合产物直接失败，不会把旧二进制标记成当前源码。Rust 变更后重新执行 `build:wasm`。

应用的 `worker/sdk/index.ts` 是 workspace 内的窄 facade，直接引用 SDK 现有源码与 WASM transport，不要求新增 SDK 公共 `./workers` 入口或专用构建步骤；不是独立发布 SDK 的用法。执行准入 digest 使用原生 `node:crypto`，Workers 配置启用 `nodejs_compat`，不能以 browser crypto polyfill 替代。测试用 `Bun.build` 的 `target: "bun"` 保留原生 crypto 并执行准入行为；这不表示 Worker 会运行 Bun/stdio 主 SDK。Turso 查询始终显式传入服务端 token，不依赖 Bun 或 process 环境变量回退。

## 目录职责

- `shared/`：Zod schema、SDK 同步帧的文本适配与 Yjs 展示边界转换；不重复实现 SDK 投影、序号或状态向量规则。
- `worker/api/`：Hono app、HTTP 边界与可注入 repository 的路由入口。
- `worker/chat/`：会话查询、ACP 创建、聊天 DO 生命周期、启动与聊天路由。
- `worker/execution/`：SDK 准入/控制 dispatcher 和 DO 事务存储 adapter。
- `worker/instances/`：实例监控页的只读聚合，按并发上限扇出读取各聊天 DO 的资源观测。
- `worker/wasm/`：Host、配置与准备的 WASM 产物；`worker/sdk/` 是本地 SDK facade，`worker/types.ts` 保存应用契约。
- `web/api/`：带 Bearer 鉴权的 HTTP 命令/查询与只读 WebSocket 同步客户端；`web/chat/` 管理聊天状态与消息展示，`web/settings/` 管理配置界面，`web/instances/` 是只读实例监控页。

## 库与自有逻辑

- Hono 的 `bearerAuth`、`bodyLimit`、`validator` 与 `HTTPException` 替代手写鉴权、请求体读取和 HTTP 错误类。JSON 上限为实际流入的 64 KiB；边界删除 `Content-Length` 后按流计数，不信任伪造的长度。Zod 校验标题、消息与 UUID，再把类型化输入交给领域层。
- Zod 是共享 DTO 的唯一 schema 来源；浏览器校验列表、历史和同步帧，不以 TypeScript 静态类型代替外部输入检查。
- 现有 SDK `SessionDocs` 负责 ACP → Yjs 投影，`SessionDocSync` 负责批处理、generation、sequence、状态向量补齐和慢订阅者预算，`SessionDocReplica` 校验并应用浏览器副本。使用 SDK 已有的只读复制协议，不引入可写回的通用协作文档服务，也不维护另一套文本 delta 聚合器。`lib0/buffer` 将 SDK 二进制更新转换为 Base64，WebSocket 仅承载这些帧；原 SSE 客户端和 `eventsource-parser` 已移除。
- 展示文本直接读取 SDK view；session 文档的 `peri-cf` map 只保存聊天元数据、执行标志和展示 ID/时间/错误，不在每个增量重复保存完整消息数组。客户端不能写回 Yjs；生成和停止仍由鉴权 HTTP 命令进入服务端控制边界。
- TanStack React Query 管理只读列表与历史缓存、加载及查询取消；客户端按认证工作区隔离，token 不进入 query key，切换 token 会卸载旧工作区并取消、清空旧缓存。禁用自动重试和自动重取；新建后显式刷新，历史导航强制查询权威结果。
- 生成、exact-target Stop、等待停止确认和 own-stop Resume 保留显式领域状态机，不交给查询重试或缓存规则。Markdown 展示通过 React `lazy` / `Suspense` 按需加载，避免增加首屏负担。

创建未提交的 `.dev.vars`：

```dotenv
APP_AUTH_TOKEN=replace-with-a-long-random-local-token
PERI_MACHINE_ID=replace-with-a-stable-deployment-uuid
PERI_STORAGE_URL=libsql://your-database.turso.io
PERI_STORAGE_TOKEN=replace-with-your-turso-token
MODEL_BASE_URL=https://api.openai.com/v1
MODEL_API_KEY=replace-with-your-model-key
MODEL_ID=your-model-id
MODEL_PROVIDER=openai
```

`MODEL_PROVIDER` 可为 `openai`（默认）或 `anthropic`，须与服务端地址使用的协议一致。复用 SDK demo 的 Anthropic 配置时，将 `ANTHROPIC_BASE_URL` / `ANTHROPIC_API_KEY` 映射到模型变量，将 `PERI_TURSO_URL` / `PERI_TURSO_AUTH_TOKEN` 映射到存储变量；不要把提供商密钥当作网页访问令牌。

为该部署生成一次 UUID（例如 `bun -e 'console.log(crypto.randomUUID())'`），替换 `PERI_MACHINE_ID` 占位值并持久保存。该部署的聊天及 Host 重启复用同一身份，不要每次启动随机生成。独立部署不要复制别的部署的机器身份；更换身份不是执行接管或恢复手段。

```bash
bun install
bun run prepare:wasm
bun run dev
```

打开 Vite 输出的本地地址，在前端输入 app token。本地 DO 由 Wrangler/workerd 模拟，无需创建 D1 数据库或运行数据库迁移脚本；上面的 Turso 和模型配置仍会访问外部服务并可能产生费用，不会自动启动本地模型或 Turso。不要把 `.dev.vars`、Turso token 或模型密钥提交到仓库。`dev` / `build` 脚本会再次准备 WASM，显式准备一步有助于提前定位缺失产物。

WASM 的平台适配留在本应用：准备阶段为 Emscripten glue 注入静态 Node TCP 模块、受支持的 DNS record resolver，并跳过 Workers 不支持的本机 socket bind；即时调度使用 timer task，避免微任务循环阻塞外部 I/O。每个 Host 使用独立 WASM 实例，不跨 HTTP/DO 请求共享带 I/O 的运行时。SDK 原始产物不被改写，glue 形状变更会让准备步骤明确失败。

`bun run dev` 构建应用，再由 Wrangler 在本地 workerd 启动编译后的 Worker，默认地址为 `http://127.0.0.1:8791`；已有构建可直接 `bun run preview`。启动时仅在服务端用本机 DNS 解析配置的存储与模型域名，将临时 `PERI_DNS_OVERRIDES` 写入忽略提交的构建目录 `.dev.vars`。不会改动源 `.dev.vars` 或生产配置，部署时须重新执行干净构建，生产环境使用 Workers DNS resolver；本地映射随开发服务重启刷新。

默认启动不使用 Vite preview：2026-10-07 对相同产物和配置的对照探测，Vite preview 在 ACP 已创建会话后的 TS 元数据查询阶段返回 `Network connection lost.`，而独立 Wrangler 查询及关闭均返回 200、创建返回 201。没有重试创建来掩盖错误；完整真实浏览器复验记录在 `E2E-RESULTS.md`。`dev:hmr` 仍是显式的 Vite 开发入口，不作为完整 runtime 验收路径。

`bun run dev:hmr` 保留 Vite HMR 调试入口，但真实外部存储验证中其 ModuleRunner 路径出现了列表 500 和 I/O 请求错误，不作为完整 WASM 聊天的验收入口。默认本地启动绕过这条路径；直接 `preview` 不负责生成本地 DNS 绑定，不能据此宣称部署网络已验收。

真实浏览器验收使用 `bun run test:e2e`，覆盖 UI 登录、多轮回答、Yjs/WS 同步、刷新历史、停止后继续与移动端布局，不 mock 模型或数据库。浏览器安装、费用及运行条件见 [E2E 指南](E2E.md)。

当前真实配置曾完整通过一轮，但连续复跑仍存在存储 I/O 间歇失败，尚不能宣称稳定可用；已验证的修复和未解决的问题见 [E2E 结果](E2E-RESULTS.md)。

## API

HTTP 查询和命令均需 Bearer token。WebSocket 在升级后通过第一帧鉴权，鉴权前不发送文档；令牌不放在 URL 中。

| 方法 | 路径 | 请求 / 响应 |
| --- | --- | --- |
| GET | `/api/chats` | JSON `{ chats: Chat[] }` |
| POST | `/api/chats` | JSON `{ "title": "可选标题" }`（也可 `{}`），201 返回裸 `Chat` |
| GET | `/api/chats/:id` | JSON `{ chat, messages }` |
| POST | `/api/chats/:id/messages` | JSON `{ "content": "你好" }`，202 接受执行；不表示执行完成 |
| POST | `/api/chats/:id/cancel` | 请求取消当前生成，JSON `{ cancelled: boolean }` |
| GET | `/api/chats/:id/resources` | JSON `AgentResources`，见 Agent 实例资源查询 |
| GET | `/api/instances` | JSON `{ generatedAt, instances: ChatInstanceObservation[] }`，见实例监控页面 |
| GET / WS | `/api/chats/:id/sync` | 首帧鉴权后接收 SDK Yjs snapshot / update |

`Chat` 为 `{ id, title, updatedAt }`。消息为 `{ id, role, content, createdAt, status, error? }`，`status` 为 `running`、`completed`、`cancelled` 或 `error`。聊天 ID 使用 UUID；忙碌返回 409，未鉴权返回 401，未配置 app token 返回 503。创建、发送需要 `Content-Type: application/json`；无效内容返回 400。

列表与创建不访问聊天 DO。创建通过独立的 WASM Host 执行 ACP `initialize` → `session/new` → `session/rename`，关闭 Host 后读取 Turso 中已落盘的摘要，再返回真实 Session ID；此时无需发送第一条消息，列表即可看到新聊天。创建不发 prompt、不启动 execution dispatcher；创建失败、Store 中不可见或关闭未确认不算成功。查询失败也不会当成空列表掩盖。

WS 首帧为 `{ type: "auth", token, resume? }`，`resume` 是 SDK 副本的实际状态向量。服务端回复 `{ type: "snapshot", snapshot }`，后续推送 `{ type: "update", update }`；文档字节在 wire 上为 Base64，协议版本仍为 SDK 的 2。断线后同代 producer 可补 delta；DO 重建的不同 generation 必须替换快照，不能合并旧副本。序号缺口交由 SDK 检测，重新订阅修复，不重发生成命令。

WS 是只读展示通道，不等于任务所有权：关闭页面或 socket 不自动取消正在执行的任务；重新连接读取当前状态。只有取消命令和服务端生命周期规则能停止执行。鉴权/只读违规与会话不存在不自动重连；临时断线重连只恢复订阅。生成完成、失败、部分回复取消均通过同步的 SDK view 与执行标志展示，202 或 socket 关闭均不是停止证明。

## 远端部署

1. 登录 Cloudflare（`bunx wrangler login`），核对部署账号与环境；无需创建 D1 数据库。
2. 核对 `CHAT_SESSIONS` binding、`ChatSession` class 名和 SQLite DO migration。首次部署应用配置中的 `new_sqlite_classes`；已部署项目后续 DO 类变更须按实际部署历史追加 migration，不可重写已有 tag。这是 DO 类部署迁移，不是单独执行的 SQL 迁移脚本。
3. 通过 Wrangler secrets 设置凭证，勿放入 `vars` 或前端构建：

```bash
bunx wrangler secret put APP_AUTH_TOKEN
bunx wrangler secret put PERI_STORAGE_URL
bunx wrangler secret put PERI_STORAGE_TOKEN
bunx wrangler secret put MODEL_API_KEY
```

4. 在 Worker 配置中设置稳定的 `PERI_MACHINE_ID` UUID，以及正确的 `MODEL_BASE_URL`、`MODEL_ID`，并确保 Worker 能访问 Turso 和模型端点。部署更新与重启保留该部署的 UUID；生产推荐配置 Cloudflare Access，并验证其保护范围及身份策略。
5. 在本目录执行：

```bash
bun run deploy
```

DO 部署迁移不会迁移 Turso Rust Store；聊天 DO 的执行账本/展示状态与 Rust Store 没有跨存储原子事务，元数据不再单独写入索引。部署也不会自动证明模型凭证、网络、CPU/内存预算或长时间 WebSocket 正常。上线前实际验证鉴权、创建/列表、连续聊天、取消、失败状态、断线重连及 DO 重启后的 Session 加载。运行诊断保留原始错误及 cause，不主动输出环境凭据；部署端须控制诊断接口及日志访问权限。

WebSocket 使用 DO Hibernation API、持久 socket attachment 和事件入口恢复鉴权及只读订阅。浏览器在应用 SDK 文档帧后发送 delivery ACK；服务端按未确认字节、帧数和截止时间关闭慢客户端，不依赖 workerd 不提供的 `bufferedAmount`。运行中的 Host、网络及收尾仍会阻止休眠，不承诺生成过程无驻留成本，也不把休眠恢复等同于计算接管。

## WASM 限制与验证范围

WASM 运行的是 ACP Host，不是本地终端工具环境：没有宿主项目文件系统、任意 shell/子进程或本地 builtin MCP 服务。默认关闭工具能力；虚拟工作区不能当作用户磁盘。若以后接入远程 MCP，需独立设计授权、环境生命周期和网络可达性。

CF feature 的模型 HTTP/SSE 经原生 Worker Fetch adapter，提供商规则仍由 Rust 管理；Rust Store 的 Turso 路径仍依赖现有网络 adapter，不把模型 Fetch 的通过当成 Store 网络问题已解决。命令型 hook 在 Emscripten 下明确报错，不再静默放行；Skill preload 的 registry 查询不使用无工作线程环境下的 `spawn_blocking`。

Fetch adapter 接受当前提供商使用的缓冲 JSON 请求体；响应队列最多两块 64 KiB，原始读取块超过 1 MiB 明确拒绝。取消、调用 future 或 body 丢弃时中断网络并释放 reader；清理错误保留原始诊断。重定向使用 manual，不跨域自动转发凭据；只有请求显式配置 timeout 时才应用全请求 deadline，未配置不虚构默认值。传输契约可在仓库根目录复验：

```bash
EMSDK_PYTHON=$(command -v python3) ./scripts/cargo-wasm.sh build --locked --release --target wasm32-unknown-emscripten -p peri-model --features cloudflare --example cloudflare-contract
node scripts/test-cloudflare-fetch.mjs
```

该 runner 使用真实 Emscripten 模块及受控 Fetch fixture，验证状态/请求头/流、错误 cause、取消、丢弃、超时及队列边界，不调用付费模型，也不代替真实 workerd 和远端 Store 验收。

每个 CF WASM 线性内存最大 64 MiB，栈维持 8 MiB，增长步长 1 MiB；同一 isolate 只预留一个最大尺寸 Host，并保留 64 MiB JS/运行时预算。预算是保守准入策略，不是平台 RSS 测量。仅确认原生关闭、释放和自有网络清理后释放预留；未知收尾阻止新 Host。启动取消使用 AbortSignal，超时不等于实际退出。

默认 `cf-wasm-size` 与可选 `cf-wasm-speed` 构建 profile 可分别通过 `bun run build:wasm --profile <name>` 构建，再用 `bun run wasm:profiles` 对比实际产物体积和构建耗时；该报告不声称 runtime CPU 或吞吐收益。流式投影只在结构变化时刷新元数据，在持久化/读取边界生成完整历史；广播帧复用编码，不为每个客户端反复解码重编码。

DO 的持久化不等于在途计算恢复：实例重启或断线不能承诺自动继续模型请求、无损重放或跨实例执行接管。Workers 的 WASM bundle、CPU、内存和连接限制需在实际部署套餐下验收。

本应用的执行 adapter 面向根聊天会话的完整执行准入，不是通用 `ManagedAgents` / `Agent` SDK 宿主，也不提供 child runtime 或子 Agent 接管能力。执行器须先封闭新准入，并在确认 ACP Host 关闭后记录停止；未知是否停止不能因为 DO 有持久化账本就自动解锁或重新执行。

运行中取消通过 SDK `SessionControl` 读取控制 snapshot，提交带 `{ turnId, attemptId }` 精确目标及 lifecycle/revision/control generation 的 typed Stop；结果未知时只解析原命令，拒绝或未知结果不算取消成功。启动中尚未发送 prompt 时只跳过本轮，不发送无目标的 Stop。

取消接口等待执行收尾、ACP Host 关闭与消息落盘；成功不是仅提交 Stop。若 Host 关闭未确认，聊天会持久化为禁止继续执行，重新加载 DO 也不会绕过该状态；历史仍可读取。不要通过删除状态或重启强行认领未知是否停止的执行。

本应用自己的 Stop 所产生的 pause marker（lifecycle、control generation、稳定 resume command ID）持久化在聊天 DO 中；仅在用户明确发送下一条消息时，核对该 pause 的匹配身份并提交稳定 Resume 命令，再发送 prompt。读历史或重建 DO 不自动 Resume；外部 pause、已改变的控制代际或恢复失败不得当作本应用的暂停自行解锁。

本示例不提供无限历史：新消息准入时，已有历史加本轮用户消息/助手占位记录的序列化大小不得超过 1 MiB，超限返回 413，应创建新聊天；单轮助手输出按 JSON 编码后文本计算限制为 512 KiB，超限取消执行并保留已接受的部分回复，以 `error` 终结。最终记录可能大于准入时的 1 MiB，不应把该值理解为数据库总容量。

```bash
bun run test
bun run typecheck
bun run build
```

测试入口只在本包执行，不属于根 Cargo workspace 测试：

- `tests/backend.test.ts`：真实 `fetchApi` / `ChatSessionCore`，注入线程 repository、模拟 ACP transport、fake execution dispatcher factory 和复制隔离的内存 DO storage；另有浏览器 `ChatApi` 经 Fetch 的 Request/Response 接入真实路由的闭环，无需监听本地端口。覆盖鉴权、创建/列表不访问 DO、GET 权威元数据刷新、load-only 实际会话身份、查询失败、数据库不可用时取消，以及 202、busy、部分回复、own-stop 恢复与原始错误因果链保留。fake dispatcher 不执行 SDK 准入规则，不能据此宣称 ticket/admission 或 durable ledger 已验证；这些规则由本包的真实 SDK 行为测试及实际 runtime smoke 验证。
- `tests/web-api.test.ts`：直接执行前端 HTTP API，验证 Bearer、响应 schema、会话身份与 `202 {accepted:true}`；接受请求不等于生成完成，命令不重试。
- `tests/sync-wire.test.ts`：真实 SDK producer/replica/view 经应用 wire 适配复制，覆盖 Unicode、状态向量 delta、序号缺口、新 generation 替换、应用收尾失败与帧大小/格式预算。
- `tests/server-sync.test.ts`：真实 DO 核心与 SDK 同步，注入 socket seam，覆盖首帧鉴权前无文档/数据库访问、超时、连接上限、只读违规、背压、断线不取消、补齐与重建历史。
- `tests/client-sync.test.ts`：真实 SDK 副本与可控 WebSocket，验证首帧鉴权、双文档 observer 重绑定、增量修复、身份隔离、限次重连、就绪超时，以及导航/发送/停止生命周期不重放命令。
- `tests/http-middleware.test.ts`：验证 Hono JSON 校验、UTF-8 字节限制，以及伪造或缺失 Content-Length 不能绕过请求体上限。
- `tests/chat-queries.test.ts`：验证私有 QueryClient、认证缓存隔离、读请求取消、强制历史刷新和禁用自动重试；不代替实际 UI 生命周期验收。
- `tests/repository.test.ts`：直接执行 `TursoChatRepository`，注入 SDK storage 的只读 seam 与 ACP 创建函数，验证工作区范围、实际会话 ID、存储顺序、标题与更新时间、重建后刷新、创建后可见性及错误传播；不是源文件文本断言。
- `tests/session-creation.test.ts`：执行真实创建服务，注入 ACP transport，验证初始化协议、UUID、rename 回执、失败关闭、关闭未确认和反向能力拒绝；创建不发 prompt、不访问聊天 DO。
- `tests/wasm-config.test.ts`：执行配置构建函数，验证外部 Turso、模型配置、必需变量和关闭工具能力。
- `tests/execution.test.ts`：直接执行 DO execution storage adapter、WorkerExecution 与真实可移植 SDK 准入/控制服务。验证串行/回滚隔离 fixture、CAS 竞争、原命令 claim 重放、registry 重建、admit/entered/settle、seal、typed Stop 的精确目标、Unknown 解析原命令、拒绝结果及精确 generation 停止证明。Host 与控制 snapshot 是 fixture，只有模拟 close 成功才提供停止确认；不以账本存在代替真实 Host 退出证明，也不证明 Cloudflare 平台事务或真实 WASM 已验收。

- `tests/execution-admission-core.test.ts`：从 SDK 迁入，执行共享准入规则、重建、冲突、预算与证据丢失行为，并实际加载应用 facade bundle 执行 admit/entered/settle，不以源码文本判断 crypto 可用。
- `tests/storage-turso-workers.test.ts`：从 SDK 迁入，加载 facade bundle，在没有 Bun/process 全局的 Node 子进程里以显式 token 经原生 Fetch 查询本地 HTTP fixture，验证 canonical SQL、元数据、错误传播与连接关闭；不假设 SDK 支持 process 环境变量回退。

多数测试不启动真实 WASM 或 Cloudflare DO，也不调用远端模型或 Turso；原生 Fetch 测试会启动本地 HTTP fixture。`tests/real-workerd-ws.test.ts` 单独运行真实 workerd 和 Hibernation socket，但使用模拟 ACP transport，不证明真实 WASM。内存 DO fixture 不是平台持久性证明。真实运行时可参考 [SDK Workers 示例](../@peri-sdk/examples/workers/worker.js)，但该示例的结果不能代替本应用上线验收。覆盖范围与实际执行结果以 `tests/` 和命令输出为准；尚未验证真实模型上线，也不宣称生产完成。

## 可重复本地 Runtime Smoke

本目录提供 `scripts/smoke.mjs`，通过 `smoke:local` 先构建应用，再启动真实本地 sqld、模拟模型 HTTP endpoint 和 Wrangler/workerd。需要 Node（Wrangler 使用）、Bun、经过 provenance 校验的 CF WASM 产物，以及可执行的 `sqld`；不是 `bun test` 的默认依赖。

```bash
# sqld 已在 PATH 中
bun run smoke:local

# 或显式指定可执行文件，无需通过 mise exec
SQLD_BIN=/absolute/path/to/sqld bun run smoke:local
```

脚本使用临时本地数据库与测试凭证，检查新 Store 的空列表、创建后首条 prompt 之前的会话可见性，以及直接 SQL 修改标题后列表和已初始化聊天 DO 的 GET 元数据刷新。通过真实 WebSocket 首帧鉴权，应用 SDK 副本并读取 view；消息命令检查 HTTP 202，生成结果从 Yjs 同步获取，不用轮询代替 WS 验收。执行两轮真实 WASM/ACP 调用，停止和重新启动 Wrangler，检查聊天 DO 历史与 ACP 加载后的模型上下文恢复。随后启动第三轮，收到部分回复后请求 exact-target typed Stop，检查取消成功及部分回复持久化；显式发送第四轮，核对 own-stop Resume 后正常完成，最后清理子进程和临时资源。模型是 fixture，不调用真实模型服务；sqld 是本机 Rust Store，不是托管 Turso。此 smoke 不验证远端部署、真实模型质量或生产网络故障。

**验收状态：直读 Turso 版本的本地 fixture smoke 已通过。** 2026-10-06 主线程确认当前 WASM/Worker 在真实本地 workerd 下通过：`passed: true`、`wasm: true`、`turns: 3`、`primaryModelCalls: 4`、`predictionCalls: 4`、`historyAfterRestart: true`、`acpContextAfterRestart: true`、`cancelledTurn: true`、`partialReplyPersisted: true`、`continuedAfterCancel: true`，以及 `tursoDirectory: true`、`listBeforeFirstPrompt: true`、`externalTitleVisible: true`。其中 `tursoDirectory` 是 smoke 的直读 Store 列表检查标签，不代表另有目录 DO。新会话首条 prompt 前已可列出；直接 SQL 改名后，列表和已初始化聊天 DO 的 GET 均返回新标题。`turns: 3` 指三轮正常完成，取消的一轮另行记录。Wrangler 重启后，展示历史与 ACP 模型上下文保留；第三轮取消保留部分回复，第四轮显式发送经 own-stop Resume 后完成。主模型调用与辅助预测调用分别按 fixture 计数，预测结果不进入用户回答，也不作为重复用户执行统计。

此结果证明上述本地执行、重启后加载和正常取消路径，不证明在途执行恢复、全部取消失败路径或未知停止后的安全接管。模型仍是本地 fixture，存储仍是本机 sqld，未验证 hosted Cloudflare、真实模型或托管 Turso，不宣称生产完成。后续变更仍应以完整 smoke 命令的退出码、断言结果与运行日志复验，不把构建成功等同于 runtime smoke 成功。

2026-10-06 当前版本在独立 worktree 从源码重新构建 SDK/Rust WASM 与应用后，完整复验通过，额外结果为 `webSocketSync: true`、`commandHttp202: true`。本包 281 项测试、类型检查和构建通过；SDK `./test/*.test.ts` 的 90 项测试及类型检查通过。SDK 源码变更仅保留准入核心与 SQLite 装配的两文件隔离，Yjs/WS 接线没有继续修改 SDK，浏览器复用已有公共 `./view` 入口。完整 SDK 集成套件不是本包验收范围：新 worktree 未构建本机 `target/debug/peri`；SDK 另有未改动的后台任务断言，期望 `awaiting_input` 而现状为 `unobserved:awaiting_input`，不能据上述定向通过宣称整个 SDK 全量通过。SDK 测试使用项目 mise 工具时，新 worktree 需信任该受控配置或设置进程级 `MISE_TRUSTED_CONFIG_PATHS`。

当前构建产物另经 headless Chromium、模拟 HTTP API 与真实本地 Yjs/WS fixture 检查：实际 App 切换 token 后不显示旧列表和历史，按需加载 Markdown、正常生成、状态向量重连不重发消息且不自动取消、部分回复停止时等待服务端回执、停止后可继续发送，以及 390px 移动布局均通过，无页面异常。这是 UI/fixture 验证，不是远端服务验收。当前构建有 Zod 上游 PURE 注解告警，未阻断构建，也未修改依赖源码。Cloudflare 类型使用局部 type import，避免 Worker 全局声明覆盖 Node/Bun 工具类型；原生 Fetch fixture 显式移除 Node 的浏览器 Web Storage 全局以模拟 Worker 环境，不屏蔽未知 stderr。
