# @peri-code/peri-cf

Peri WASM Agent 的 Bun 长驻服务 + 浏览器 Chat。服务端持有 `ChatManager`、`ChatService`、
`PeriInstance`、`ChatView` 与 WASM ACP Host；浏览器只消费目录、HTTP Action 回执与只读 Yjs 副本。
模型、Store 与 MCP 凭据只存在于服务端环境变量。设计见
[服务端 Agent](../../docs/design/peri-cf-server-agent.md) 与
[Chat 前端](../../docs/design/peri-cf-chat-ui.md)；入口见
[peri-cf 代码索引](../../docs/code-index/peri-cf.md)。

## 生产测试场

peri-cf 是 `@peri-code/sdk` 与 `peri-sandbox` 的生产测试场：SDK 固定为 registry 上的
已发布版本，`bun install && bun run typecheck && bun test` 直接验证发布产物（入口、类型
与 WASM 资源），而不是工作区源码。升级 SDK 是显式动作：修改 `package.json` 中的版本号
并重新安装，禁止改回 `file:` 链接。`peri-sandbox` 尚未发布，暂以
`file:../peri-sandbox` 链接；每个实际会话启动一个真实 sandbox（见
[每会话 Sandbox](#每会话-sandbox)），`server/sandbox-adapter.test.ts` 另外校验 adapter
与已发布 SDK 的 `/sandbox` 契约。

## 架构

```text
浏览器
  → frontend/remote：HTTP Action + WebSocket 二进制同步
  → server/api：目录 / 句柄 / Action / payload / WS upgrade
  → @peri-code/sdk：ChatManager → ChatService → PeriInstance → ACP
  → peri-wasm Host（每次激活一个）→ 模型 / Turso Store / 远程 MCP
  → peri-sandbox（每会话一个）→ 本地工作区 / 文件与终端 MCP
```

- **服务端**：`server/index.ts` 是 Bun 入口；`server/compose.ts` 装配 `chatManagerOptions`；
  `server/registry.ts` 的 `ChatRegistry` 持有 `ChatManagerContract` 与 `ChatViewRegistry`，
  编排 View 生命周期与关闭/停机；`server/api/index.ts` 只做 HTTP 边界校验与错误
  映射；`server/api/sync.ts` 把 Bun WebSocket 适配为 SDK `ChatYjsChannel`。
- **前端**：`frontend/remote/controller.ts` 是页面控制器；`frontend/chat/runtime/useChat.ts` 装配控制器、
  目录查询与载荷读取；`frontend/chat/` 是展示外壳与时间线。
- **同步**：客户端只发 HELLO/ACK；服务端发 SNAPSHOT/UPDATE；客户端不能写 Y.Doc。慢客户端由 SDK
  交付信用/背压显式关闭。
- **打开历史**：先 `POST /api/chats`（历史会话带 `sessionId`，新建会话不带 body）得到空句柄，
  再 `change_session`；202 后才连 WS。已有控制者返回 `binding_conflict`，前端改用 Session ID
  寻址既有句柄。
- **每会话 sandbox**：新建 Chat 启动新实例；打开历史按会话 cwd 找回同一 sandbox；旧 `/workspace`
  会话不启动 sandbox。租约按 `sandboxId` 引用计数，关闭 Chat 释放但保留工作区，停机 `closeAll`。
- **运行态**：列表只返回 Store 目录事实；选中会话的 running/交互/队列只来自副本，不从列表推断。
- **没有访问控制**：服务不提供认证、限流或多租户隔离；入口鉴权是部署必要条件。

## Generative UI (OpenUI)

- 模型默认继续用 Markdown。只有结构化视觉明显更好时，才输出**一个**标记为 `openui` 的围栏代码块，内含完整程序，首行 `root = Stack([...])`；解释文字在围栏外。
- 前端只认顶层完整围栏；流式中未闭合围栏显示“正在生成界面…”骨架，不显示半截代码。解析失败、root 存在但 prop 校验失败/引用未解析、`meta.incomplete`、懒加载或渲染异常都会回退到 Markdown 原文，用户始终能看到模型输出。流结束后，整段以 `root = Stack(` 开头的裸 DSL 也会被识别；普通代码块或列表项内的 `openui` 字样不算。
- OpenUI 组件库与 spec 独立在 `npm-packages/peri-openui`（`file:` 依赖）。唯一事实源是 `../peri-openui/src/library.tsx`；`bun run openui:spec` 转发到 `../peri-openui/scripts/generate.ts` 生成 `../peri-openui/spec.json`，`bun run openui:check` 校验新鲜度；`bun run build` 已前置该检查。
- 服务端 `server/openui-instructions.ts` 只经 `peri-openui/prompt` 读取 spec 生成提示词，不引入 React/canvas。提示词随 `session/new` 冻结，**旧会话不会获得 OpenUI 段**，需要新建会话。
- 本地 `frontend/chat/openui/` 只保留聊天适配：`OpenUIMessage`、`actions`、`index`。门面从 `peri-openui` 取 `splitOpenUI`/主题，懒加载 `peri-openui/renderer`。
- 表单值不持久化，刷新/切换会话即重置。`Button` 的 `continue_conversation` 复用 `chat.send`；`open_url` 只允许无 userinfo、长度 ≤2048 的 `http(s)` 链接。超长消息/表单数据会在发送文本中标注截断。
- OpenUI 只在懒加载 chunk：主 chunk 不含 `react-lang`、`LineChart`、`BarChart`；构建输出 `renderer-*.js` 约 257 kB，另有 5 kB 的 `ci-info` 子 chunk（不阻塞构建）。`vite.config.ts` 用 `resolve.dedupe` 保证库与主应用共用一份 React。
- 组件来自 `peri-canvas@0.1.0` 的复制改写。上游 `package.json` 声明 MIT，但仓库根无 LICENSE，上游 README 自述为某商用 Canvas SDK 的移植；授权链不明，仅限内部使用，对外分发前必须法律复核。详见 `../peri-openui/PROVENANCE.md`。

## 每会话 Sandbox

- **一个实际会话一个实例**：`POST /api/chats` 不带 `sessionId`/`path` 时先启动新 sandbox，再把它的
  `workspace` 与 MCP 描述注入 `createChat`；随后 `create_new_session` 的 cwd 落在该工作区。
- **打开历史**：带 `sessionId` 时服务端从 Store 读取会话 `cwd`，解析 `<root>/<sandboxId>/workspace`，
  启动或复用同一 sandbox；解析不到的旧 `/workspace` 会话继续使用固定工作区。
- **工具面**：sandbox 以 MCP 名称 `sandbox` 合并进该 Chat 的 Session 配置，不覆盖 `PERI_MCP_SERVERS`。
  远程 MCP 不得使用该名称。不能改名为 ACP 保留的 `workspace`：宿主对该名称要求资源发现，
  而 sandbox MCP 只提供工具，`resources/list` 的 `-32601` 会阻断整个会话。
  凭据只在服务端，不进入浏览器。
- **生命周期**：租约按 `sandboxId` 引用计数；关闭 Chat 释放租约、保留工作区，下次打开重新启动；
  停机先释放视图与 Chat，再 `closeAll`。启动失败返回 503 `sandbox_start_failed`。
- **目录**：会话目录合并固定工作区与 `<root>/sbx_<32hex>/workspace` 下的 Store 查询。
- **前置条件**：`peri-sandbox` 的 `dist` 必须已构建；sandbox 需要 Peri 可执行文件与
  `@anthropic-ai/sandbox-runtime`（Linux 另需 user systemd/cgroup）。根 `postinstall` 会执行
  peri-sandbox 的 `patch-sdk`，使 MCP 任务结果兼容当前 2.4.0 SDK。

## HTTP / WS

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/monitor` | 只读监控快照：LLM / MCP / WASM / Sandbox；地址去敏，header 只给名称 |
| GET | `/api/chats` | Store 目录；不可达 503 + `available:false` |
| POST | `/api/chats` | 按目标启动/复用 sandbox 并登记句柄，不创建 Session；body 可带 `sessionId` 或 `path`（互斥） |
| POST | `/api/chats/:chatId/actions` | `ChatUserAction`；202 回执 |
| DELETE | `/api/chats/:chatId` | 关闭句柄并释放 View；成功 204，未确认 500；View 释放失败后可用同一 `chatId` 重试 |
| GET | `/api/chats/:chatId/payload/:refId?bytes=` | 按引用读取工具全文 |
| WS | `/api/chats/:chatId/sync` | 只读 Yjs 同步；未知 Chat 升级前 410 |

Action 封套为 `{ ...action, target: { sessionId, bindingRevision } }`；空句柄用
`{sessionId:null,bindingRevision:0}`。`send` 回执的 `accepted` 是入队，不表示已投递；`cancel` 的
`requested` 不是 stopped。动作失败以 `disposition`（`rejected`/`unknown`/`not_sent`）表达可重试性，
HTTP 状态不单独决定重试。加载路径的绑定冲突映射 409 `binding_conflict`，KV 读/写失败映射 500 `claim_failed`；
新会话 claim 阶段失败（本地/KV 冲突或 KV 读/写失败）映射 500 `session_creation_failed`（stage `claim`）。
没有通用 503 ownership 错误码；503 只用于 sandbox 启动失败（`sandbox_start_failed`）。

## 环境变量

| 变量 | 必填 | 说明 |
| --- | --- | --- |
| `PERI_MODEL_PROVIDER` | 否 | `openai`（默认）或 `anthropic` |
| `PERI_MODEL_ID` | 是 | 主模型 ID，同时是四档未单独配置时的回退值 |
| `PERI_MODEL_ID_FABLE` / `_OPUS` / `_SONNET` / `_HAIKU` | 否 | 各档模型 ID；缺省回退 `PERI_MODEL_ID` |
| `PERI_MODEL_BASE_URL` | 是 | http(s) 上游；Anthropic 填 `https://api.anthropic.com`，SDK 追加 `/v1/messages` |
| `PERI_MODEL_API_KEY` | 是 | 模型凭据，只在服务端 |
| `PERI_STORAGE_URL` | 是 | Turso/libSQL http(s) 地址 |
| `PERI_STORAGE_TOKEN` | 是 | Store 凭据，只在服务端 |
| `PERI_MACHINE_ID` | 是 | 持久 UUID；服务端不随机生成 |
| `PERI_MCP_SERVERS` | 否 | JSON 数组：`[{name,url,token?,headers?}]`，只接受 http(s)；名称 `sandbox` 保留给会话 sandbox；`workspace` 是 ACP 保留名 |
| `PERI_SANDBOX_ENABLED` | 否 | 默认 `true`；`false` 回退固定 `/workspace`，不启动 sandbox |
| `PERI_SANDBOX_ROOT` | 否 | 绝对路径，默认 `~/.peri-sandbox/sandboxes`；工作区为 `<root>/<sandboxId>/workspace` |
| `PERI_SANDBOX_SETTINGS` | 否 | 绝对路径；peri-sandbox 设置文件 |
| `PERI_SANDBOX_PROFILE` | 否 | 传给 `startSandbox` 的 profile 名 |
| `PERI_PATH` | 否 | sandbox 使用的 Peri 可执行文件，默认 `~/.peri/peri` |
| `PERI_SERVER_HOST` / `PERI_SERVER_PORT` | 否 | 默认 `127.0.0.1:8787` |
| `PERI_STATIC_ROOT` | 否 | 前端静态目录，默认 `dist/client` |
| `PERI_WASM_MODULE_URL` | 否 | WASM 胶水位置（file/http），默认 `wasm/generated/peri-wasm.js` |
| Langfuse 键 | 否 | 只转发 `peri-config` 实际读取的键（`LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_BASE_URL` 及可选调优键）；未读取的同前缀键（如 `LANGFUSE_TRACING`）不传给 WASM 实例 |

任何配置解析问题都会拒绝启动并一次性报告缺失项；密钥不进入前端、不写日志。

四档示例（Anthropic 官方端点；实际请求发送到 `https://api.anthropic.com/v1/messages`）：

```bash
PERI_MODEL_PROVIDER=anthropic
PERI_MODEL_BASE_URL=https://api.anthropic.com
PERI_MODEL_API_KEY=...
PERI_MODEL_ID=claude-sonnet-5-5
PERI_MODEL_ID_FABLE=claude-fable-5-1
PERI_MODEL_ID_OPUS=claude-opus-5-5
PERI_MODEL_ID_SONNET=claude-sonnet-5-5
PERI_MODEL_ID_HAIKU=claude-haiku-5-5
```

## 本地运行

```bash
cd npm-packages/peri-openui
bun install                                  # OpenUI 库与 spec 生成器自身依赖
cd ../peri-cf
bun install
bun run dev                                  # prepare:wasm + Bun 服务 + Vite
# 或
bun run start                                # 需要已构建的 wasm/generated 与 dist/client
```

SDK 来自 registry，安装即得到发布产物；CF 的 WASM 产物仍由本仓库的 Rust/Emscripten
工具链构建（`build:wasm`/`prepare:wasm`，依赖 Emscripten 6.0.10）。`scripts/wasm-tools.ts`
从 `node_modules` 读取已安装 SDK 的 `exports["./wasm"]` 校验入口，不再读取工作区兄弟
目录。`peri-openui` 与 `peri-sandbox` 都是 `file:` 依赖：`bun install` 只链接工作区包目录，
包自身依赖需先在各包目录安装（`peri-openui` 如上；`peri-sandbox` 首次运行前需在
`npm-packages/peri-sandbox` 执行
`bun run build`（其 `dist` 不入库）。CF 的根 `postinstall` 会运行该依赖的 `patch-sdk`，
为 MCP 2.4.0 打任务结果补丁；`bun install --ignore-scripts` 后需手动执行
`node node_modules/peri-sandbox/scripts/patch-sdk.mjs`。

## 验证

```bash
bun run openui:check
bun run typecheck
bun test
./node_modules/.bin/vite build
bun run build        # openui:check + prepare:wasm + Vite；需要 WASM 工具链
```

- `bun test` 覆盖 frontend、scripts 与 server；真实模型/Store/远程 MCP 不在单测内。`bunfig.toml`
  的 test preload 为链接包 `peri-openui` 做 React 去重（浏览器构建由 Vite `resolve.dedupe` 处理）。
- OpenUI spec 过期时 `openui:check` 直接失败；修改 `../peri-openui/src/library.tsx` 后在该包运行
  `bun run spec` 并提交 `spec.json`。
- 浏览器包检查：`dist/client` 不应出现 `PERI_MODEL_API_KEY`、`PERI_STORAGE_TOKEN`、`node:fs` 或
  `@tursodatabase`。
- 真实运行后端验收需要匹配源码的 WASM 产物与可用模型/Store；未验证平台必须标记 blocked。

## 部署限制

- 单进程 ownership：生产固定 `createStorage()`（unstorage memory driver）作为 `kv`，由 `ChatManager` 内部构造协调器；多实例部署需要改造 `server/compose.ts` 接入共享 storage，并由路由层做串行化/崩溃清理；不能宣称跨实例接管。
- 进程退出会结束在途执行；历史仍在 Store，可重新加载续聊，但不恢复旧执行。
- 工具面由远程 Streamable HTTP MCP 与会话 sandbox 的 MCP 组成；WASM 进程内仍没有文件/终端 builtin。
- hosted Workers/DO 路径已删除；Bun 服务是唯一后端入口。没有认证、限流或多租户隔离。
