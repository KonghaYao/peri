# @peri-code/peri-cf

Peri WASM Agent 的 Bun 长驻服务 + 浏览器 Chat。服务端持有 `ChatManager`、`ChatService`、
`PeriInstance`、`ChatView` 与 WASM ACP Host；浏览器只消费目录、HTTP Action 回执与只读 Yjs 副本。
模型、Store 与 MCP 凭据只存在于服务端环境变量。设计见
[服务端 Agent](../../docs/design/peri-cf-server-agent.md) 与
[Chat 前端](../../docs/design/peri-cf-chat-ui.md)；入口见
[peri-cf 代码索引](../../docs/code-index/peri-cf.md)。

## 架构

```text
浏览器
  → frontend/remote：HTTP Action + WebSocket 二进制同步
  → server/api：目录 / 句柄 / Action / payload / WS upgrade
  → @peri-code/sdk：ChatManager → ChatService → PeriInstance → ACP
  → peri-wasm Host（每次激活一个）→ 模型 / Turso Store / 远程 MCP
```

- **服务端**：`server/index.ts` 是 Bun 入口；`server/compose.ts` 是平台装配（唯一构造具体
  `ChatManager` 的位置）；`server/host.ts` 的 `ChatHost` 持有 `ChatManagerContract` 与
  `ChatViewRegistry`，编排句柄转发、View 生命周期与关闭；`server/api/index.ts` 只做 HTTP 边界校验与错误
  映射；`server/api/sync.ts` 把 Bun WebSocket 适配为 SDK `ChatYjsChannel`。
- **前端**：`frontend/remote/controller.ts` 是页面控制器；`frontend/chat/runtime/useChat.ts` 装配控制器、
  目录查询与载荷读取；`frontend/chat/` 是展示外壳与时间线。
- **同步**：客户端只发 HELLO/ACK；服务端发 SNAPSHOT/UPDATE；客户端不能写 Y.Doc。慢客户端由 SDK
  交付信用/背压显式关闭。
- **打开历史**：先 `POST /api/chats` 得到空句柄，再 `change_session`；202 后才连 WS。已有控制者返回
  `binding_conflict`，前端改用 Session ID 寻址既有句柄。
- **运行态**：列表只返回 Store 目录事实；选中会话的 running/交互/队列只来自副本，不从列表推断。
- **没有访问控制**：服务不提供认证、限流或多租户隔离；入口鉴权是部署必要条件。

## HTTP / WS

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/info` | 去敏后的服务信息 |
| GET | `/api/chats` | Store 目录；不可达 503 + `available:false` |
| POST | `/api/chats` | 只登记句柄，不创建 Session |
| POST | `/api/chats/:chatId/actions` | `ChatUserAction`；202 回执 |
| DELETE | `/api/chats/:chatId` | 关闭句柄并释放 View；成功 204，未确认 500；View 释放失败后可用同一 `chatId` 重试 |
| GET | `/api/chats/:chatId/payload/:refId?bytes=` | 按引用读取工具全文 |
| WS | `/api/chats/:chatId/sync` | 只读 Yjs 同步；未知 Chat 升级前 410 |

Action 封套为 `{ ...action, target: { sessionId, bindingRevision } }`；空句柄用
`{sessionId:null,bindingRevision:0}`。`send` 回执的 `accepted` 是入队，不表示已投递；`cancel` 的
`requested` 不是 stopped。动作失败以 `disposition`（`rejected`/`unknown`/`not_sent`）表达可重试性，
HTTP 状态不单独决定重试。加载路径的绑定冲突映射 409 `binding_conflict`，KV 读/写失败映射 500 `claim_failed`；
新会话 claim 阶段失败（本地/KV 冲突或 KV 读/写失败）映射 500 `session_creation_failed`（stage `claim`）。
没有通用 503 ownership 错误码；本轮不新增 503。

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
| `PERI_MCP_SERVERS` | 否 | JSON 数组：`[{name,url,token?,headers?}]`，只接受 http(s) |
| `PERI_SERVER_HOST` / `PERI_SERVER_PORT` | 否 | 默认 `127.0.0.1:8787` |
| `PERI_STATIC_ROOT` | 否 | 前端静态目录，默认 `dist/client` |
| `PERI_WASM_MODULE_URL` | 否 | WASM 胶水位置（file/http），默认 `wasm/generated/peri-wasm.js` |
| `LANGFUSE_*` | 否 | 只有此前缀的变量会交给 WASM 实例 |

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
cd npm-packages/@peri-sdk && bun run build   # 先构建 SDK（含 peri-wasm）
cd npm-packages/peri-cf
bun install
bun run dev                                  # prepare:wasm + Bun 服务 + Vite
# 或
bun run start                                # 需要已构建的 wasm/generated 与 dist/client
```

SDK 构建依赖 Emscripten 6.0.10，需在 `npm-packages/@peri-sdk` 执行
`MISE_ENV=wasm mise exec -- bun run build`（`mise exec` 注入 `EMSDK`）。CF 通过
`file:../@peri-sdk` 解析 `node_modules/@peri-code/sdk`；执行 `bun install --force` 后该条目为指向
工作区包的符号链接，CF 直接消费最新 `dist`。干净 CI 顺序：SDK build → CF `bun install --force` →
`bun run typecheck && bun test`；不要手工复制 `node_modules` 副本。

## 验证

```bash
bun run typecheck
bun test
./node_modules/.bin/vite build
bun run build        # prepare:wasm + Vite；需要 WASM 工具链
```

- `bun test` 覆盖 core、frontend、scripts 与 server；真实模型/Store/远程 MCP 不在单测内。
- 浏览器包检查：`dist/client` 不应出现 `PERI_MODEL_API_KEY`、`PERI_STORAGE_TOKEN`、`node:fs` 或
  `@tursodatabase`。
- 真实运行后端验收需要匹配源码的 WASM 产物与可用模型/Store；未验证平台必须标记 blocked。

## 部署限制

- 单进程 ownership：生产固定 `createStorage()`（unstorage memory driver）作为 `kv`，由 `ChatManager` 内部构造协调器；多实例部署需要改造 `server/compose.ts` 接入共享 storage，并由路由层做串行化/崩溃清理；不能宣称跨实例接管。
- 进程退出会结束在途执行；历史仍在 Store，可重新加载续聊，但不恢复旧执行。
- 工具面只保留远程 Streamable HTTP MCP；WASM 部署没有进程内文件/终端 builtin。
- hosted Workers/DO 路径已删除；Bun 服务是唯一后端入口。没有认证、限流或多租户隔离。
