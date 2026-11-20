# `/v1` 资源接口

本文面向已经拿到发行包、要在本机写客户端程序的外部开发者。它只描述 HTTP 契约：如何启动、如何设置 `Authorization`、如何用 npm 包 `openai` 的客户端对象发起请求，以及每个资源族的端点、字段和本地降级行为。

当前部署注册 **57 个操作 / 34 条路径**（按代码中的路由表统计）。所有路径都在 `/v1` 前缀下。

## 1. 启动与地址

`/v1` 由应用进程直接提供，启动命令取决于你手上的目录：

| 场景 | 启动 | 实际运行 |
| --- | --- | --- |
| 源码工作目录（仓库 checkout 或副本） | `bun run start` | `server/main.ts`；`scripts/api.sh` 在 shell 未设置 `RIVETKIT_STORAGE_PATH` 时把它导出为包根相对的 `.data/rivet` |
| 安装后的 kit（四个 tarball 的安装目录） | `./node_modules/.bin/agent-builder start` | 包内 `dist/server/main.js` |

源码工作目录里没有可执行的 `./node_modules/.bin/agent-builder`；安装后的 `bin/agent-builder.js` 也只加载 `dist`。在源码里改完代码后必须 `bun run build:server` 重建 `dist/server/main.js`，否则进程继续跑旧产物（`bun run build:web` 只重建前端）。因此 `/v1` 只存在于跑本仓库源码、或已重建 `dist` 的进程里。doctor 也一样：安装后的 kit 用 `./node_modules/.bin/agent-builder doctor`，源码目录需要先有 `dist/server/main.js`，再执行 `bun bin/agent-builder.js doctor`。

默认地址是 `http://127.0.0.1:3000`，API 根路径是 `http://127.0.0.1:3000/v1`。`PORT` 可以改端口；`HOST` 固定为 `127.0.0.1`，服务不会监听其他网卡。API 与 Web 页面同源，没有单独的 API 进程。

**`RIVETKIT_STORAGE_PATH` 的优先级取决于启动方式。** `scripts/api.sh` 在启动 Bun 前解析该变量：shell 中已设置的非空值优先（相对路径按包根解析），否则使用包根下的 `.data/rivet`，然后 `export`。由于 shell 环境优先于 `.env`，且导出发生在 Bun 加载 `.env` 之前，`bun run start` 下 `.env` 里的 `RIVETKIT_STORAGE_PATH` **不会**生效。安装后的 launcher 不同：未设置时默认 `<DATABASE_PATH 目录>/rivet`，显式环境变量或 `.env` 值优先，相对路径按启动目录解析，并对该目录做与 `DATABASE_PATH` 相同的祖先符号链接检查。直接运行 `bun server/main.ts`（不经 `scripts/api.sh`）时没有脚本默认值，`.env` 生效，相对路径按启动目录解析。

启动前需要按 README 配好 Store、workspace 等部署配置。`/v1` 本身不管理 Provider：通过同源管理接口 `POST /api/providers` 配置**恰好一条** Provider 记录（Web 的 Providers 页调用同一接口）并保存凭据；本流程创建的 Agent 未指定 Provider，会话执行依赖这条记录。该接口不需要 `Authorization`，只接受回环 Host 与同源 Origin；最小步骤见 §2。检查部署状态可以请求 `/api/health`（不带 `/v1` 前缀，不需要 `Authorization`）。

### 1.1 Authorization

每个 `/v1` 请求都必须带 bearer 头：

```
Authorization: Bearer local-dev-key
```

本部署只检查这个头**是否存在且形如 `Bearer <非空值>`**，不校验 token 内容、不区分租户、不记录 token。任意非空值都可以通过；缺失或使用其他 scheme 返回 401 `authentication_required`。

这不是身份认证边界。服务只监听回环地址，不要把端口转发或暴露到其他机器。

### 1.2 请求与响应

JSON 请求体必须带 `Content-Type: application/json`。对象是 strict 的：出现未定义字段返回 400 `invalid_input`，不会静默忽略。

成功响应的形状按资源族区分，**不是所有** JSON 都带 `object`：

- 顶层资源（例如 Agent、Session、Turn、Subagent、Artifact、Trace、Environment、Template、Vault、Credential、Automation、Invocation）带 `object` 判别字段。
- session item 不带 `object`，用 `type` 区分种类（`message`、`reasoning`、`function_call` 等，见 4.4）。
- ID 游标列表统一返回 `object: "list"` 信封；环境文件列表是例外，返回 `object: "page"`（见 4.9）。
- 删除成功返回 `{ "id": "...", "object": "<资源类型>.deleted", "deleted": true }`。
- SSE 事件和 `202` 空响应体不适用上述规则，分别按 4.5 和具体端点定义。

### 1.3 部署前提与启动陷阱

**会话执行前提。** 对于未指定 Provider 的 Agent（`/v1` 创建的 Agent 默认如此），Session 执行要求部署里恰好有一条 Provider 记录（且该记录保存了凭据），且 Store 可达（`PERI_STORE_URL` + `PERI_STORE_TOKEN`）。这类 Agent 在零条或多条 Provider 时创建 Session 返回 400 `invalid_input`；Store 认证/连接失败或引擎未就绪时返回 503 `engine_unavailable`。Provider 通过同源管理接口 `/api/providers` 配置（Web 管理页使用同一接口），`/v1` 没有 Provider 端点；该接口不需要 `Authorization`，但只接受回环 Host 与同源 Origin。`/api/health` 的 `ok` 只表示 HTTP 进程存活并能响应，`engine.status` 才是运行时是否就绪的直接信号；`ok:true` 且 `engine.status` 非 `ready` 是正常组合。`GET /api/capabilities` 的 `chatRuntimeReady` 与之一致（只在 `ready` 时为 `true`），`chatRuntime` 只是配置模式（`wasm` / `not_configured`）。

**`doctor` 的边界。** `agent-builder doctor` 只校验配置是否存在（例如是否缺 Store token、WASM 资源是否安装），不发起 Store 请求、不启动引擎、不读取 Provider 凭据。`doctor` 通过不代表 Store 或引擎可达，也不代表 Provider 的 key 可用。

**`PERI_STORE_URL` 的形状取决于所连服务。** 这个值原样交给 libSQL HTTP 客户端，应用不解析也不改写。sqld 的 namespace 通常按端口区分，URL 用 origin 形式（如 `http://127.0.0.1:<port>`）；带 `/db/<namespace>/<database>/<branch>` 的路径形式只适用于支持按路径寻址的实现，并非所有 Store 都接受。以实际部署的服务文档为准；拿不准时先用能连通的最小 URL 验证。

**绝对路径要用 realpath 后的真实路径。** macOS 上 `/tmp` 是指向 `/private/tmp` 的符号链接。`DATABASE_PATH` 的目录会逐级检查既有祖先，符号链接或非目录组件会被拒绝；`PERI_WORKSPACE_PATH` 要求路径本身不是 symlink 且 `realpath` 后与原文一致（祖先含 symlink 会得到 `workspace_path_not_canonical`）；安装后的 launcher 还会对 `RIVETKIT_STORAGE_PATH` 做同样的祖先检查。应用不会替你 realpath 归一化（workspace URI 依赖原样路径），所以绝对路径请先 `realpath` 再写进配置。

**端口规划。** 内嵌引擎除 `PERI_ENGINE_PORT`（默认 8961）外还会占用其 +1 端口（默认 8962），两个端口都必须空闲。`PORT`（默认 3000）是 Web/API 端口，与引擎端口无关。任一端口被占用时，`/api/health` 的引擎状态会停在非 ready，错误信息通常不直接点名端口；用 `lsof -nP -iTCP:<port> -sTCP:LISTEN` 检查这两个引擎端口。

## 2. 最小可运行客户端

下面这条链路完成：建 Agent → 建 Session → 提交一条输入 → 读取 SSE 事件直到会话空闲 → 取 turn 和 item。

**先配置 Provider（无浏览器）。** `/v1` 没有 Provider 端点（`POST /v1/providers` 返回 404）；Provider 由同一进程的 `/api/providers` 管理，Web 的 Providers 页也调用它。本流程创建的 Agent 未指定 Provider，会话执行要求部署里恰好一条 Provider 记录，且该记录保存了凭据，因此远端/无浏览器集成者可以先完成这一步。

把请求体写入权限 0600 的临时文件，再用 `--data-binary @file` 发送，使 curl 的命令行参数中只有文件名：

```bash
PROVIDER_BODY="$(mktemp)"
chmod 600 "$PROVIDER_BODY"
cat > "$PROVIDER_BODY" <<'JSON'
{
  "name": "local-default",
  "type": "<PROVIDER_TYPE>",
  "baseUrl": null,
  "credential": { "action": "replace", "value": "<PROVIDER_API_KEY>" }
}
JSON
curl -sS -X POST http://127.0.0.1:3000/api/providers \
  -H 'content-type: application/json' \
  --data-binary @"$PROVIDER_BODY"
rm -f "$PROVIDER_BODY"
```

`<PROVIDER_TYPE>` 取 `GET /api/capabilities` 的 `providers[].type`（创建后不可改）；`<PROVIDER_API_KEY>` 只是占位符。`--data-binary @file` 只把文件名放进 curl 的进程参数，curl 阶段 key 不进入命令行。临时文件仍是明文，用完必须删除；上面的 heredoc 也只写占位符，真实 key 要用编辑器或密码管理器写入该文件，或先 `read -rs` 读入变量后生成 JSON，不要直接敲进 heredoc 或命令行。成功返回 `201`，形如：

```json
{"provider":{"id":"<uuid>","name":"local-default","type":"<PROVIDER_TYPE>","baseUrl":null,"credentialSource":"local","revision":1,"hasCredential":true,"agentCount":0,"runtimeCount":0}}
```

用 `GET /api/providers` 确认列表里只有这一条且 `hasCredential` 为 `true`。若已有多条，用 `DELETE /api/providers/<PROVIDER_UUID>` 加 `If-Match: "<revision>"` 删除多余的；被 Agent 或运行引用时返回 409 `provider_in_use`。`/api/providers` 不需要 `Authorization`，但只接受回环 Host 和同源 Origin；不要把 3000 端口暴露到其他机器。

准备一个空目录和客户端包：

```bash
mkdir agents-client && cd agents-client
bun init -y
bun add openai@7.60.0
```

把下面的代码保存为 `client.ts`。把 `AGENTS_MODEL` 设成你配置的 Provider 实际提供的模型名。

```ts
import OpenAI from 'openai';

type Json = Record<string, any>;

const BASE_URL = process.env.AGENTS_API_BASE_URL ?? 'http://127.0.0.1:3000/v1';
const API_KEY = process.env.AGENTS_API_KEY ?? 'local-dev-key';
const MODEL = process.env.AGENTS_MODEL;
if (MODEL === undefined || MODEL.length === 0) {
  throw new Error('Set AGENTS_MODEL to a model name served by the configured Provider');
}

const client = new OpenAI({
  apiKey: API_KEY,
  baseURL: BASE_URL,
  maxRetries: 0,
  timeout: 120_000,
});

async function* readEvents(sessionId: string): AsyncGenerator<Json> {
  const response = await client
    .get(`/agents/sessions/${sessionId}/events`, {
      headers: { Accept: 'text/event-stream' },
      signal: AbortSignal.timeout(120_000),
    })
    .asResponse();
  if (!response.ok || response.body === null) {
    throw new Error(`SSE failed: HTTP ${response.status} ${await response.text()}`);
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) return;
      buffer += decoder.decode(value, { stream: true });

      let boundary: number;
      while ((boundary = buffer.indexOf('\n\n')) !== -1) {
        const frame = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        const data = frame
          .split('\n')
          .filter((line) => line.startsWith('data: '))
          .map((line) => line.slice('data: '.length))
          .join('\n');
        if (data.length === 0) continue; // heartbeat comment
        yield JSON.parse(data) as Json;
      }
    }
  } finally {
    await reader.cancel().catch(() => undefined);
  }
}

const agent = await client.post<Json>('/agents', {
  body: {
    name: 'my-first-agent',
    model: MODEL,
    instructions: 'Reply briefly.',
  },
});

const session = await client.post<Json>('/agents/sessions', {
  body: {
    agent_id: agent.id,
    environment: { type: 'local' },
  },
});

const accepted = await client
  .post(`/agents/sessions/${session.id}/events`, {
    body: {
      events: [
        {
          type: 'agent.session.input.message',
          input: [
            {
              role: 'user',
              content: [{ type: 'input_text', text: 'Say pong.' }],
            },
          ],
        },
      ],
    },
    headers: { 'Idempotency-Key': 'first-input-1' },
  })
  .asResponse();
if (accepted.status !== 202) {
  throw new Error(`input was not accepted: HTTP ${accepted.status} ${await accepted.text()}`);
}

const seen: string[] = [];
let reply = '';
for await (const event of readEvents(session.id)) {
  seen.push(String(event.type));
  if (event.type === 'agent.session.turn.output_text.delta') reply += String(event.delta);
  if (event.type === 'agent.session.idle') break;
}

const turns = await client.get<{ data: Json[] }>(`/agents/sessions/${session.id}/turns`, {
  query: { limit: 20 },
});
if (turns.data.length === 0) throw new Error('the session produced no turn');
const turn = turns.data[0]!;
const detail = await client.get<Json>(`/agents/sessions/${session.id}/turns/${turn.id}`);
const items = await client.get<{ data: Json[] }>(
  `/agents/sessions/${session.id}/turns/${turn.id}/items`,
  { query: { limit: 100 } },
);
const assistant = items.data.find(
  (item) => item.type === 'message' && item.role === 'assistant',
);
const text = (assistant?.content ?? [])
  .filter((part: Json) => part.type === 'output_text')
  .map((part: Json) => part.text)
  .join('');

console.log('agent  ', agent.id, agent.object, agent.model);
console.log('session', session.id, 'events', seen.length, 'last', seen.at(-1));
console.log('turn   ', detail.id, detail.status, 'total_tokens', detail.usage?.total_tokens ?? 'n/a');
console.log('items  ', items.data.map((item) => item.type).join(','));
console.log('reply  ', text || reply);
```

运行：

```bash
AGENTS_MODEL=<你的模型名> bun client.ts
```

如果服务不在默认地址：

```bash
AGENTS_API_BASE_URL=http://127.0.0.1:3100/v1 AGENTS_MODEL=<你的模型名> bun client.ts
```

输出结构如下；ID、token 数和模型回复取决于部署：

```
agent   <uuid> agent <model>
session <uuid> events <n> last agent.session.idle
turn    <uuid> completed total_tokens <n>
items   message,message
reply   <模型回复文本>
```

示例没有删除资源。需要清理时，在会话空闲后依次调用 `DELETE /agents/sessions/{session_id}` 和 `DELETE /agents/{agent_id}`。

## 3. 通用约定

### 3.1 资源 ID

资源 ID 是 36 字符、带连字符的 UUID 字符串。服务端新建的 Agent、Session、Turn、Item、Vault、Credential、Environment、Template、Automation、Invocation 和 Event 使用 UUIDv7（时间前缀，字典序与创建顺序一致）。子 agent ID 来自运行时，不要依赖版本或排序。

把 ID 当作不透明字符串保存和比较。SSE 帧的 `id:` 是十进制日志位置，不是资源 ID；`data.event_id` 才是公开事件 ID。

### 3.2 时间戳

响应中的时间戳都是 **Unix 秒**（整数），不是毫秒。适用字段包括 `created_at`、`updated_at`、`started_at`、`completed_at`、`opened_at`、`closed_at`、`last_active_at`、`run_at`、`scheduled_at`。

请求中的 `run_at` 同样是秒。不适用时字段为 `null`。

### 3.3 分页

ID 游标列表统一使用：

| 查询参数 | 规则 |
| --- | --- |
| `limit` | 1–100，默认 20；大于 100 按 100 处理；非整数或小于 1 返回 400 |
| `order` | `asc` 或 `desc`，默认 `desc` |
| `after` | 上一页的 `last_id`；排他游标；未知 ID 返回空页 |

信封：

```json
{
  "object": "list",
  "data": [],
  "first_id": null,
  "last_id": null,
  "has_more": false
}
```

`data` 为空时 `first_id` 和 `last_id` 为 `null`。环境文件的列表是唯一例外，见 4.9。

### 3.4 错误信封

错误统一为：

```json
{
  "error": {
    "type": "invalid_request_error",
    "code": "invalid_input",
    "message": "Request validation failed",
    "param": "limit"
  }
}
```

`type` 由状态码决定：

| 状态 | `type` |
| --- | --- |
| 400 / 422 | `invalid_request_error` |
| 401 | `authentication_error` |
| 403 | `permission_error` |
| 404 | `not_found_error` |
| 409 | `conflict_error` |
| 429 | `rate_limit_error` |
| 500 / 502 / 503 | `server_error` |
| 501 | `not_implemented_error` |

`message` 是固定文案，不要拿来分支；用 `code` 和 HTTP 状态判断。`param` 是出错字段的路径，可能为 `null`。

常见 `code`：

| 状态 | `code` | 场景 |
| --- | --- | --- |
| 400 | `invalid_input` | 校验失败、未知字段、空白输入、非法分页参数 |
| 401 | `authentication_required` | 缺少 bearer 头或 scheme 不是 Bearer |
| 404 | `not_found` | 资源不存在或不属于父资源 |
| 409 | `agent_has_sessions` | Agent 仍被 Session 引用，不能删除 |
| 409 | `session_active` | Session 的 turn 仍在执行，不能删除 |
| 409 | `idempotency_conflict` | 同一 `Idempotency-Key` 配了不同请求体 |
| 409 | `store_schema_unsupported` | 外部会话存储的 schema 与子 agent 历史不匹配 |
| 409 | `run_at_locked` | 已有排期调用，不能再改一次性 `run_at` |
| 409 | `run_at_required` | 恢复一次性自动化时没有未来 `run_at` |
| 400 | `environment_capability_unsupported` | 本地 workspace 不支持非空 `capability_directories` |
| 400 | `environment_file_reference_unsupported` | 只支持内联文件，不支持 `file_id` |
| 400 | `environment_file_content_invalid` | `data` 不是标准 base64 |
| 400 | `environment_file_too_large` | 解码后超过 1 MiB |
| 400 | `environment_path_invalid` | 文件路径越界或含非法字符 |
| 400 | `environment_path_symlink` | 路径经过符号链接 |
| 400 | `workspace_not_allowed` | `workspace_directory` 或 `path` 不是部署 workspace |
| 400 | `invalid_page` | 环境文件 `page` 游标非法 |
| 400 | `credential_auth_unsupported` | 凭据 `auth.type` 不受支持 |
| 400 | `timezone_unsupported` | 自动化时区不受支持 |
| 403 | `artifact_path_unsafe` | artifact 路径不在部署 workspace 内 |
| 404 | `artifact_content_unavailable` | artifact 内容不可读 |
| 409 | `artifact_size_mismatch` | artifact 记录大小与实际文件不一致 |
| 409 | `artifact_too_large` | artifact 超过 16 MiB 下载上限 |
| 500 | `credential_unavailable` | 凭据的加密密钥不可用 |
| 500 | `internal_error` | 未分类的服务端错误 |
| 503 | `engine_unavailable` | 会话运行时未配置或不可用：`/v1` 的会话创建、输入与子 agent 历史；`/api/chats` 的创建、events 订阅、send、cancel 同码，`/api` 错误体的 `details.reasons` 为稳定原因码（`/v1` 信封没有 `details`） |
| 503 | `subagent_history_unavailable` | 子 agent 历史读取不可用 |

### 3.5 幂等

只有 `POST /agents/sessions/{session_id}/events` 接受 `Idempotency-Key`：

- 长度 1–256；超出范围返回 400，`param` 为 `Idempotency-Key`。
- 作用域是单个 Session。
- 同一 key + 相同请求体重试不会重复提交，仍返回 202（空响应体）。
- 同一 key + 不同请求体返回 409 `idempotency_conflict`。
- 请求失败时不保留成功记录，可以用同一个 key 重试；如果失败发生在部分事件已经执行之后，重试仍可能重复这些事件。
- 不提供 `Idempotency-Key` 时每次请求都会提交，重试可能产生重复输入。

其他端点不接受这个头。

## 4. 资源族

### 4.1 Agents

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents` | 200 | 列表；`limit` / `order` / `after` |
| POST | `/agents` | 201 | 创建；`model` 必填 |
| GET | `/agents/{agent_id}` | 200 | 单个资源 |
| POST | `/agents/{agent_id}` | 200 | 局部更新（不是 PUT/PATCH） |
| DELETE | `/agents/{agent_id}` | 200 | 仍有 Session 引用时 409 `agent_has_sessions` |

关键字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | string | UUID |
| `object` | `"agent"` | 判别字段 |
| `name` | string \| null | 未设置时为 null |
| `instructions` | string \| null | 系统指令 |
| `model` | string | 创建时必填；原样保存 |
| `metadata` | object | 最多 16 对；键 1–64 字符，值最多 512 字符 |
| `reasoning` | object | `{effort, summary}`，未设置时字段为 null |
| `text` | object | `{format:{type:"text"}\|{type:"json_schema",schema}, verbosity}` |
| `tools` | array | 见下方工具类型 |
| `multi_agent` | object | `{enabled, max_concurrent_subagents}`；启用且未指定并发数时为 6 |
| `compaction` | object | `{async}` |
| `experimental` | object | `{time_awareness}` |
| `service_tier` | string | 默认 `"auto"` |
| `created_at` / `updated_at` | number | Unix 秒 |

`tools[].type` 支持 `function`、`tool_search`、`programmatic_tool_calling`、`mcp`、`web_search`、`computer_use`、`automations`。其中 `computer_use` 和 `function` 会被保存，但本地运行时不执行；创建或读取这类 Agent 时响应带：

```
x-platform-degraded: computer_use,function
```

列表不包含会话内联 Agent 的占位行。会话资源里的 `agent` 是快照；内联会话的 `agent.id` 不是可通过 `/agents/{id}` 读取的资源。

创建示例：

```json
{
  "name": "support-agent",
  "model": "<你的模型名>",
  "instructions": "Reply briefly.",
  "metadata": { "team": "support" }
}
```

更新是局部更新：省略的字段保持原值，显式 `null` 重置可空字段。`model` 更新只影响后续 turn。

### 4.2 Sessions

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions` | 200 | 列表；支持 `agent_id`、`metadata[key]=value`、`limit` / `order` / `after` |
| POST | `/agents/sessions` | 201 | 创建；`stream:true` 时返回 201 SSE |
| GET | `/agents/sessions/{session_id}` | 200 | 单个资源 |
| POST | `/agents/sessions/{session_id}` | 200 | 局部更新（不是 PUT/PATCH） |
| DELETE | `/agents/sessions/{session_id}` | 200 | 运行中返回 409 `session_active` |

创建请求体：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `environment` | 是 | 托管类型；见下表 |
| `agent_id` | 否 | 已保存的 Agent ID |
| `agent` | 否 | 内联覆盖：`model`、`instructions`、`reasoning`、`text`、`service_tier`、`tools`、`multi_agent`、`compaction`、`experimental` |
| `input` | 否 | 字符串，或用户消息数组；`environment.type` 为 `none` 时必填 |
| `vault_ids` | 否 | 必须引用已存在的 Vault |
| `metadata` | 否 | 最多 16 对 |
| `prompt_cache_options` | 否 | `{ttl?}`，原样保存 |
| `review` | 否 | `"none"` 或 `"auto_review"` |
| `stream` | 否 | `true` 时创建响应直接是 SSE |
| `stream_options` | 否 | 仅在 `stream:true` 时允许：`{include_obfuscation?}` |

`agent_id` 与 `agent` 可以同时出现：以已保存 Agent 为底，`agent` 覆盖。只给 `agent` 时必须提供 `model`。

`environment` 支持：

```json
{ "type": "none" }
```

```json
{ "type": "local", "capability_directories": [] }
```

```json
{ "type": "self_hosted", "workspace_directory": "<部署 workspace 的绝对路径>", "capability_directories": [] }
```

其他 `type` 会被归一为 `local`，响应带 `x-platform-degraded: environment_type_mapped_to_local`。本部署不会按 `workspace_directory` 或 `capability_directories` 建立独立环境；它们不参与本地执行。

如果部署启用了 Sandbox 但尚未配置好，示例可以改用 `{ "type": "none" }`，并在创建会话时通过 `input` 提供第一条输入（`none` 要求创建时就给输入）。

创建时提交 `input` 会立即开始一个 turn。响应里的 `status` 是提交后立即读取的快照，可能仍是 `idle`；以事件或 turns 为准。

`stream:true` 时：

- 响应状态 201，`Content-Type: text/event-stream; charset=utf-8`。
- 流从 `agent.session.created` 开始，随后是本次输入的 turn 事件。
- 创建响应不会丢失本次输入产生的事件。
- `stream_options.include_obfuscation` 只被接受，不会产生填充事件。

会话资源：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | string | UUID；同时也是会话 ID |
| `object` | `"agent.session"` | 判别字段 |
| `created_at` / `last_active_at` | number | Unix 秒 |
| `status` | string | `idle` / `in_progress` / `requires_action` / `failed` |
| `required_actions` | array | 待处理动作；见 4.5 |
| `error` | string \| null | 最近一次失败信息 |
| `agent` | object | Agent 快照（`id`、`name`、`model`、`instructions`、`tools` 等） |
| `environment` | object | `{type:"none"}` 或 `{type:"self_hosted", remote_url, id, workspace_directory, capability_directories}` |
| `metadata` | object | 字符串键值 |
| `vault_ids` | array | 已记录的 Vault ID |
| `usage` | object \| null | 累计 token 用量 |
| `prompt_cache_options` | any | 请求原值 |
| `review` | string \| null | 请求原值 |

`local` 或未知托管类型在响应中统一表现为 `self_hosted`，`workspace_directory` 常为空字符串；同时带 `x-platform-degraded: environment_type_mapped_to_local`。`none` 才是真正的 `{type:"none"}`。

`vault_ids` 只做存在性校验并记录在会话上；本地运行时不把凭据注入执行。

更新允许：`prompt_cache_options`（`null` 清除）、`vault_ids`（`null` 清空）、`review`、`agent`、`metadata`（`null` 重置为 `{}`）。不能修改 `environment` 或 `status`。

删除会同时删除该会话的事件日志。`in_progress` 或 `requires_action` 时先取消或等待结束。

**与 `/api/chats` 的会话边界。** `/v1` 创建会话时会同时建立 platform session 与对应 chat，chat 的 id 就是 `/v1` 的 session id。因此 `/v1` 创建的会话**会**出现在 `GET /api/chats` 的列表里（该列表没有来源过滤）；反过来 `/v1` 的会话列表只读 platform session，**看不到**通过 `POST /api/chats` 创建的对话。`/api/chats` 创建的 chat 不是 platform session，用它的 `chatId` 请求 `GET /v1/agents/sessions/{session_id}` 返回 404 `not_found`。两个 id 不同：`/api/chats` 的 `sessionId` 是 Store 会话 id，创建响应里可以为 `null`，在该 chat 首次订阅 `GET /api/chats/:chatId/events` 或首次 `POST /api/chats/:chatId/send` 后写入（`close` 不创建会话）；`/v1` 的 `session.id` 是 platform session id，并等于对应 `chatId`。两端共享同一本地存储，但列表可见性只有上述单向。

**创建失败后的 Chat 行。** 创建先写入 Chat 行，再打开 actor；打开失败会回滚该行。若回滚无法确认 actor 关闭（`actor.close()` 失败）或 Sandbox holder 未确认释放，该行会保留以便诊断，此时 `sessionId` 仍为 `null`。引擎在创建过程中失联时，`POST /api/chats` 返回 503 `engine_unavailable`（引擎在启动阶段就失败的话，创建发生在写行之前，不会留下行）；若引擎仍报告 `ready`，返回的是打开失败本身的错误，例如 500 `internal_error`。保留的行不会让后续接口退化为 500：`GET /api/chats` 仍为 200；运行时不可用期间，`GET /api/chats/:chatId/events` 与 `POST /api/chats/:chatId/send` 返回 503 `engine_unavailable`；`DELETE /api/chats/:chatId` 返回 409 `close_incomplete`（`details.actorClosed: false`；若只是 Sandbox 未确认释放，则为 `true`，原因码在 `details.sandboxErrors`）。运行时恢复（重启 API 并重新启动引擎）后可再次 `DELETE` 完成清理；服务不会为了“能删掉”而释放未确认关闭的运行。`/v1` 会话创建命中该路径（打开 chat 失败）时同样返回 503 `engine_unavailable`；platform session 行随创建失败删除，保留的 chat 行会出现在 `GET /api/chats`，处理方式与 `/api/chats` 相同（运行时恢复后可 `DELETE /api/chats/:chatId` 清理）。

### 4.3 Turns

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/turns` | 200 | 列表；默认最新在前 |
| GET | `/agents/sessions/{session_id}/turns/{turn_id}` | 200 | 单个 turn；不属于该会话返回 404 |
| GET | `/agents/sessions/{session_id}/turns/{turn_id}/items` | 200 | 该 turn 的 item 列表 |

字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `object` | `"agent.session.turn"` | 判别字段 |
| `id` | string | Turn ID |
| `session_id` | string | 所属会话 |
| `agent_id` | string | 执行该 turn 的 Agent |
| `subagent_id` | string \| null | 子 agent turn 时非空 |
| `status` | string | `queued` / `in_progress` / `waiting` / `completed` / `failed` / `cancelled` |
| `created_at` | number | Unix 秒 |
| `started_at` / `completed_at` | number \| null | Unix 秒 |
| `error` | object \| null | `{code, message}` |
| `usage` | object \| null | token 用量 |
| `prompt_cache_options` | any | 请求原值 |

### 4.4 Items

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/items` | 200 | 会话内全部 item；`limit` / `order` / `after` |

公共字段：`id`、`type`、`turn_id`、`status`。类型专属字段：

| `type` | 额外字段 |
| --- | --- |
| `message` | `role`（`user` / `assistant`）、`content`（`input_text` 或 `output_text` 部分数组） |
| `reasoning` | `summary`（`summary_text` 部分数组） |
| `function_call` | `call_id`、`name`、`arguments`、`status` |
| `command_execution` | `command`、`cwd`、`output`、`exit_code`、`duration_ms`、`status` |

列表项可能带运行期附加字段；只依赖上面列出的字段和 `id`/`type`/`turn_id`，忽略其他字段。默认 `order=desc`，所以最新 turn 的 item 在前。用户输入的文本不保证回显在 item 中；assistant 的文本在 `message.content[].type == "output_text"` 的 `text` 里。

### 4.5 Events

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/events` | 200 | SSE 事件流 |
| POST | `/agents/sessions/{session_id}/events` | 202 | 提交输入；空响应体 |

#### SSE 帧

```
id: 42
event: agent.session.turn.output_text.delta
data: {"type":"agent.session.turn.output_text.delta","event_id":"...","session_id":"...","turn_id":"...","item_id":"...","output_index":0,"content_index":0,"delta":"pong"}

```

- `id:` 是十进制日志位置。
- `event:` 与 `data.type` 相同。
- `data` 是单行 JSON。
- 空行分隔帧；服务端会周期性发送 `: ping` 注释行作为心跳。
- 流不会因为一个 turn 结束而关闭；客户端读到 `agent.session.idle` 后自行断开。
- 默认从日志起点重放，然后继续推送实时事件。

#### Last-Event-ID 重放

请求头 `Last-Event-ID` 可以是：

- 上一帧的 `id:`（十进制日志位置），或
- 任意事件的 `data.event_id`。

服务端从该位置之后继续。未知值按 0 处理，即从头重放。断开重连时把最后收到的 `id:` 原样带回即可。

#### POST 输入

```json
{
  "events": [
    {
      "type": "agent.session.input.message",
      "input": [
        {
          "role": "user",
          "content": [{ "type": "input_text", "text": "Continue." }]
        }
      ]
    }
  ]
}
```

支持的 `events[].type`：

| `type` | 字段 | 行为 |
| --- | --- | --- |
| `agent.session.input.message` | `input`：用户消息数组 | 提交一轮新输入 |
| `agent.session.input.cancel` | 无 | 取消当前 turn |
| `agent.session.input.mcp_approval_request_result` | `request_id`、`response:{type:"mcp_tool_call",decision}` | 回应待审批动作；`decision` 为 `approve` / `deny` / `cancel` |

`events` 为空数组时返回 202，不产生动作。其他 `type` 返回 400。输入只使用 `input_text` 部分；文本为空或超过 32,768 字符返回 400。

`required_actions` 可能出现 `function_call`、`mcp_approval_request`、`environment_connection`。本部署的输入事件只能回应 `mcp_approval_request`；`function_call` 没有对应的提交类型。

#### 事件类型

会话级事件带 `session` 对象：

| `type` | 说明 |
| --- | --- |
| `agent.session.created` | 会话已创建 |
| `agent.session.in_progress` | 开始执行 |
| `agent.session.idle` | 当前没有运行中的 turn |
| `agent.session.requires_action` | 需要审批或外部动作 |
| `agent.session.failed` | 会话失败 |

Turn 事件带 `session_id`、`turn_id`、`turn`：

| `type` | 说明 |
| --- | --- |
| `agent.session.turn.created` | turn 已创建 |
| `agent.session.turn.in_progress` | turn 开始 |
| `agent.session.turn.completed` | turn 完成；终态事件还带 `usage` |
| `agent.session.turn.failed` | turn 失败；带 `usage` |
| `agent.session.turn.cancelled` | turn 已取消；带 `usage` |

Item 与文本事件：

| `type` | 主要字段 |
| --- | --- |
| `agent.session.turn.item.added` | `output_index`、`item` |
| `agent.session.turn.item.done` | `output_index`、`item` |
| `agent.session.turn.content_part.added` | `item_id`、`output_index`、`content_index`、`part` |
| `agent.session.turn.content_part.done` | 同上 |
| `agent.session.turn.output_text.delta` | `item_id`、`output_index`、`content_index`、`delta` |
| `agent.session.turn.output_text.done` | `item_id`、`output_index`、`content_index`、`text` |
| `agent.session.turn.reasoning_summary_part.added` | `item_id`、`output_index`、`summary_index`、`part` |
| `agent.session.turn.reasoning_summary_part.done` | 同上，另带 `status` |
| `agent.session.turn.reasoning_summary_text.delta` | `item_id`、`output_index`、`summary_index`、`delta` |
| `agent.session.turn.reasoning_summary_text.done` | `item_id`、`output_index`、`summary_index`、`text` |
| `agent.output.command_execution_output.delta` | `item_id`、`output_index`、`delta` |

子 agent 事件带 `subagent` 对象：`agent.session.subagent.created`、`agent.session.subagent.active`、`agent.session.subagent.closed`。

所有事件都带 `event_id`。它可用于 `Last-Event-ID`，但不是 SSE 帧的 `id:`。

### 4.6 Subagents

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/subagents` | 200 | 子 agent 列表 |
| GET | `/agents/sessions/{session_id}/subagents/{subagent_id}` | 200 | 单个子 agent |
| GET | `/agents/sessions/{session_id}/subagents/{subagent_id}/turns` | 200 | 子 agent 的 turn 列表 |
| GET | `/agents/sessions/{session_id}/subagents/{subagent_id}/turns/{turn_id}` | 200 | 单个 turn |
| GET | `/agents/sessions/{session_id}/subagents/{subagent_id}/turns/{turn_id}/items` | 200 | 该 turn 的 item 列表 |
| GET | `/agents/sessions/{session_id}/subagents/{subagent_id}/items` | 200 | 子 agent 的全部 item |

子 agent 是 Agent 运行时产生的只读资源，没有创建或删除端点。

字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `object` | `"agent.session.subagent"` | 判别字段 |
| `id` | string | 子 agent / 子线程 ID |
| `session_id` | string | 父会话 |
| `name` | string \| null | 名称 |
| `instructions` | array \| null | 内容数组，或 null |
| `parent_agent_id` | string | 父 Agent |
| `status` | string | `active` / `closed` |
| `opened_at` | number | Unix 秒 |
| `closed_at` | number \| null | Unix 秒 |

历史来源：

- 默认从本部署记录的子 agent turn/item 读取。
- 如果部署安装了外部会话存储读取器，则以该存储为准。存储不可用时，列表端点返回 200 空页并带 `x-platform-degraded: subagent_history_unavailable`；单个 turn 返回 503 `subagent_history_unavailable`；存储 schema 不匹配时返回 409 `store_schema_unsupported`。
- 空页不会伪造 span 或历史行。

### 4.7 Artifacts

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/artifacts` | 200 | 列表；可带 `environment_id` 过滤 |
| GET | `/agents/sessions/{session_id}/artifacts/{artifact_id}` | 200 | 单个资源 |
| DELETE | `/agents/sessions/{session_id}/artifacts/{artifact_id}` | 200 | 删除记录 |
| GET | `/agents/sessions/{session_id}/artifacts/{artifact_id}/content` | 200 | 原始字节 |

字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `object` | `"agent.session.artifact"` | 判别字段 |
| `id` | string | Artifact ID |
| `session_id` | string | 所属会话 |
| `environment_id` | string | 没有关联环境时为空字符串 |
| `turn_id` | string | 没有关联 turn 时为空字符串 |
| `path` | string | 路径 |
| `size_bytes` | number | 记录的大小 |
| `created_at` | number | Unix 秒 |

本部署不从托管环境发布 artifact，正常情况下列表为空、单条读取为 404。`environment_id` 过滤对本地行没有意义：本地行的 `environment_id` 是空字符串，传非空值会得到空列表。

`content` 端点从部署 workspace 读取真实文件：

- `Content-Type` 取记录的 media type；无效或缺失时用 `application/octet-stream`。
- 路径越出部署 workspace 返回 403 `artifact_path_unsafe`。
- 内容不可读返回 404 `artifact_content_unavailable`。
- 大小与记录不一致返回 409 `artifact_size_mismatch`；超过 16 MiB 返回 409 `artifact_too_large`。

### 4.8 Traces

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/sessions/{session_id}/traces` | 200 | Trace 列表；`limit` / `order` / `after` |

字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `object` | `"agent.session.trace"` | 判别字段 |
| `id` | string | 有 turn 时是 turn ID，否则是 trace 记录自身的 ID |
| `session_id` | string | 所属会话 |
| `created_at` | number | Unix 秒 |
| `otlp` | object | Trace 文档；当作不透明对象 |

本部署通常没有 trace 发布者，列表为空。只有 payload 是 JSON 对象的记录会被返回；同一 turn 只返回一次。没有单条 trace 端点。

### 4.9 Environments

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/environments` | 200 | 列表；可带 `type` 过滤 |
| POST | `/agents/environments` | 201 | 创建 |
| GET | `/agents/environments/{environment_id}` | 200 | 单个资源 |
| GET | `/agents/environments/{environment_id}/files` | 200 | 文件列表（分页信封不同，见下） |
| POST | `/agents/environments/{environment_id}/files` | 201 | 写入内联文件 |

环境创建后没有更新端点，也没有删除端点；需要变更时新建。`DELETE /agents/environments/{environment_id}` 不在契约内，返回 404 `not_found`，环境及其文件记录保留。

创建请求体可以嵌套，也可以直接平铺：

```json
{
  "name": "primary",
  "vault_ids": [],
  "environment": {
    "type": "local",
    "workspace_directory": "<部署 workspace 的绝对路径，通常省略>",
    "environment_template_id": "<可选模板 ID>",
    "packages": { "python": ["numpy"] },
    "setup_commands": [{ "command": "echo setup", "cwd": null }],
    "network": { "access": "restricted", "allowed_domains": ["example.test"] },
    "desktop": { "enabled": true },
    "skills": [],
    "plugins": [],
    "container_size": "small",
    "files": [{ "type": "inline", "path": "notes/a.txt", "data": "<标准 base64>" }]
  }
}
```

规则：

- `type` 支持 `none`、`self_hosted`、`local`；其他值归一为 `local`，响应带 `environment_type_mapped_to_local`。`type` 缺省时为 `local`。
- `workspace_directory` 通常省略；传了就必须等于部署 workspace，否则 400 `workspace_not_allowed`。
- `capability_directories` 必须为空；非空返回 400 `environment_capability_unsupported`。
- `environment_template_id`（别名 `template_id`）会复制模板配置，再应用内联值；数组和设置以内联值替换模板值。
- `setup_commands` 和 `env` 会被接受和校验，但不会持久化、也不会回显。
- `files` 只支持 `type:"inline"`，`data` 必须是标准 base64；`file_id` 引用返回 400 `environment_file_reference_unsupported`。
- `name` 被接受，但不在响应中返回。

环境资源只暴露以下字段：

```json
{
  "id": "<uuid>",
  "object": "agent.environment",
  "type": "local",
  "status": "ready",
  "files": [
    { "type": "inline", "id": "<uuid>", "path": "/workspace/notes/a.txt", "size_bytes": 5 }
  ],
  "skills": [],
  "plugins": []
}
```

`packages`、`network`、`desktop`、`setup_commands`、`env`、`workspace_directory`、`capability_directories`、`container_size`、`name` 都不在响应中。它们要么不被本地执行，要么按配置丢弃；实际结果通过 `x-platform-degraded` 报告。

`GET /agents/environments?type=...` 的 `type` 过滤同样会把未知值映射为 `local`，并带 `environment_type_filter_mapped_to_local`。

环境文件：

- 内容保存在应用数据库中，不会写到 workspace 目录；接口不返回文件内容。
- `path` 必须是 workspace 相对路径或 `/workspace/...`；不能包含 `..`、反斜杠、控制字符，也不能是 workspace 之外的绝对路径。
- `data` 解码后最多 1 MiB。
- 同一路径再次写入会替换内容并保留原文件记录 ID。
- `GET .../files` 的 `path` 过滤是目录前缀；相对路径、`/workspace/...` 或部署 workspace 绝对路径都接受。
- 文件列表不使用 ID 游标，信封是：

```json
{
  "object": "page",
  "data": [
    {
      "object": "agent.environment.file",
      "environment_id": "<uuid>",
      "path": "/workspace/notes/a.txt",
      "size_bytes": 5
    }
  ],
  "next": "<opaque cursor or null>",
  "has_more": false
}
```

下一页把 `next` 原样作为 `page` 查询参数，并保持 `limit` / `order` / `path`。

### 4.10 Environment templates

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/environments/templates` | 200 | 列表 |
| POST | `/agents/environments/templates` | 201 | 创建 |
| GET | `/agents/environments/templates/{environment_template_id}` | 200 | 单个模板 |
| POST | `/agents/environments/templates/{environment_template_id}` | 200 | 局部更新（不是 PUT/PATCH） |
| DELETE | `/agents/environments/templates/{environment_template_id}` | 200 | 删除 |

字段：

```json
{
  "id": "<uuid>",
  "name": "base",
  "object": "agent.environment.template",
  "created_at": 1790000000,
  "updated_at": 1790000000,
  "packages": { "python": [], "system": [], "npm": [] },
  "network": { "access": "disabled", "allowed_domains": [] },
  "desktop": { "enabled": false },
  "capability_directories": [],
  "skills": [],
  "plugins": [],
  "files": [
    { "type": "inline", "path": "/workspace/seed/a.txt", "size_bytes": 13 }
  ]
}
```

- `setup_commands` 和 `env` 会被接受，但不会持久化或返回。
- `network` 响应只包含 `access` 和 `allowed_domains`。
- `files` 可以是 `inline`（带 `size_bytes`）或 `file_id`（带 `file_id`）；路径按 `/workspace/...` 返回。
- `capability_directories` 非空返回 400 `environment_capability_unsupported`。
- 更新是局部更新：省略的字段保持原值，显式 `null` 重置。
- 删除模板不影响已经创建的环境；创建时配置已经复制。

### 4.11 Vaults 与 credentials

Vault：

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/vaults` | 200 | 列表 |
| POST | `/vaults` | 201 | 创建 |
| GET | `/vaults/{vault_id}` | 200 | 单个资源 |
| POST | `/vaults/{vault_id}` | 200 | 局部更新（不是 PUT/PATCH） |
| DELETE | `/vaults/{vault_id}` | 200 | 删除 Vault 及其全部 credentials |

```json
{
  "id": "<uuid>",
  "object": "vault",
  "name": "deploy",
  "metadata": { "team": "platform" },
  "created_at": 1790000000
}
```

`POST /vaults` 的 `name` 必填，trim 后 1–256 字节；`metadata` 可选。更新时省略字段保持原值，`name:null` 清空名称（响应为 null），`metadata` 替换整个对象。

Credential：

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/vaults/{vault_id}/credentials` | 200 | 列表 |
| POST | `/vaults/{vault_id}/credentials` | 201 | 创建 |
| GET | `/vaults/{vault_id}/credentials/{credential_id}` | 200 | 单个资源 |
| POST | `/vaults/{vault_id}/credentials/{credential_id}` | 200 | 轮换秘密或更新 metadata |
| DELETE | `/vaults/{vault_id}/credentials/{credential_id}` | 200 | 删除 |

创建请求体的 `auth` 支持两种：

```json
{
  "name": "bearer",
  "auth": {
    "type": "static_bearer",
    "mcp_server_url": "http://127.0.0.1:9999/mcp",
    "token": "<write-only secret>"
  }
}
```

```json
{
  "name": "env-secret",
  "auth": {
    "type": "environment_variable",
    "secret_name": "SERVICE_TOKEN",
    "secret_value": "<write-only secret>",
    "networking": { "type": "restricted", "allowed_hosts": ["example.test"] }
  }
}
```

`environment_variable.networking` 可以是 `{ "type": "unrestricted" }` 或 `{ "type": "restricted", "allowed_hosts": [...] }`；host 只接受主机名或 IPv4，最多 16 个，不能重复。

响应中的 `auth` 只包含非秘密描述：

```json
{ "type": "static_bearer", "mcp_server_url": "http://127.0.0.1:9999/mcp" }
```

```json
{
  "type": "environment_variable",
  "secret_name": "SERVICE_TOKEN",
  "networking": { "type": "restricted", "allowed_hosts": ["example.test"] }
}
```

`token` / `secret_value` 只入不出：不会出现在任何响应、列表或错误里。credential 资源包含 `id`、`object:"vault.credential"`、`vault_id`、`name`、`auth`、`metadata`、`created_at`、`updated_at`。

更新规则：

- 请求体至少包含 `auth` 或 `metadata` 之一。
- `auth` 只用于轮换秘密，必须与存储的认证类型相同；尝试改类型返回 400 `invalid_input`。
- `static_bearer` 轮换只需要 `{ "type": "static_bearer", "token": "..." }`。
- `environment_variable` 轮换只需要 `{ "type": "environment_variable", "secret_value": "..." }`。
- `name` 不能通过更新修改。
- 加密密钥不可用时返回 500 `credential_unavailable`。

删除 Vault 会级联删除其 credentials；删除单个 credential 只影响自身。

### 4.12 Automations 与 invocations

Automation：

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/automations` | 200 | 列表；可带 `session_id` 过滤（只匹配 `existing_session` 目标） |
| POST | `/agents/automations` | 201 | 创建 |
| GET | `/agents/automations/{automation_id}` | 200 | 单个资源 |
| POST | `/agents/automations/{automation_id}` | 200 | 局部更新（不是 PUT/PATCH） |
| DELETE | `/agents/automations/{automation_id}` | 200 | 删除自动化及其 invocation 记录 |
| POST | `/agents/automations/{automation_id}/pause` | 200 | 暂停 |
| POST | `/agents/automations/{automation_id}/resume` | 200 | 恢复 |

创建 `periodic`：

```json
{
  "type": "periodic",
  "name": "daily-review",
  "expression": "0 9 * * *",
  "timezone": "UTC",
  "target": {
    "type": "existing_session",
    "session_id": "<session uuid>",
    "instructions": "Run the review."
  },
  "metadata": { "team": "platform" }
}
```

创建 `once`：

```json
{
  "type": "once",
  "name": "one-shot",
  "run_at": 4102444800,
  "target": {
    "type": "new_session",
    "agent_id": "<agent uuid>",
    "environment": { "type": "none" },
    "input": [
      { "role": "user", "content": [{ "type": "input_text", "text": "hello" }] }
    ]
  }
}
```

目标：

- `existing_session`：`session_id` 必须存在，`instructions` 必填。
- `new_session`：`environment` 必填，`input` 必须是消息数组，`agent_id` 与 `agent` 必须且只能提供一个。
- `new_session` 的响应环境是较窄的形状：
  - `{ "type": "none" }`
  - 或 `{ "type": "self_hosted", "environment_id": null, "workspace_directory": "...", "capability_directories": [] }`

未知托管类型归一为 `local`，响应带 `environment_type_mapped_to_local`。

时间规则：

- `expression` 是 5 段 cron（分 时 日 月 周）。
- `timezone` 是 IANA 时区名（例如 `UTC`、`Asia/Shanghai`）；不支持时返回 400 `timezone_unsupported`。
- `run_at` 是 Unix 秒，且必须是未来时间。
- `periodic` 只接受 `expression` / `timezone`；`once` 只接受 `run_at`。

资源字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | string | UUID |
| `object` | `"agent.automation"` | 判别字段 |
| `created_at` | number | Unix 秒 |
| `name` | string | 名称 |
| `target` | object | 见上 |
| `metadata` | object | 字符串键值 |
| `created_by` | object | `{type:"developer"}` 或 `{type:"agent",session_id,tool_call_id}` |
| `pause_reason` | string \| null | 手动暂停为 null；调度器因目标失败暂停时可能非空 |
| `status` | string | `periodic`：`active` / `paused`；`once`：`active` / `paused` / `completed` |
| `expression` / `timezone` | string | 仅 `periodic` |
| `run_at` | number | 仅 `once`，Unix 秒 |

更新是局部更新。`periodic` 可改 `name`、`target`、`metadata`、`expression`、`timezone`；`once` 可改 `name`、`target`、`metadata`、`run_at`。已经存在排期 invocation 时改 `run_at` 返回 409 `run_at_locked`。不能改类型。

`resume`：

- `periodic` 重新计算下一次执行时间。
- `once` 必须在未来有 `run_at`，或已有 queued 排期；否则 409 `run_at_required`。

Invocation：

| 方法 | 路径 | 成功 | 说明 |
| --- | --- | --- | --- |
| GET | `/agents/automations/{automation_id}/invocations` | 200 | 调用记录列表 |
| POST | `/agents/automations/{automation_id}/invocations` | 200 | 手动触发；立即返回 queued |
| GET | `/agents/automations/{automation_id}/invocations/{invocation_id}` | 200 | 单条调用记录 |

手动触发返回 200，而不是 201，响应里的 `status` 是 `queued`；投递在响应之后异步开始。需要结果时轮询单条 invocation 或列表。

字段：

```json
{
  "id": "<uuid>",
  "object": "agent.automation.invocation",
  "automation_id": "<uuid>",
  "created_at": 1790000000,
  "source": { "type": "manual" },
  "scheduled_at": null,
  "session_id": null,
  "status": "queued",
  "skip_reason": null,
  "error": null
}
```

- `source` 是 `{type:"manual"}` 或 `{type:"automation",automation:<automation 快照>}`。
- `status` 是 `queued` / `completed` / `failed` / `skipped`。
- `scheduled_at` 是排期时间（秒）；手动触发为 null。
- `session_id` 在投递完成后是目标会话；手动触发刚返回时为 null。
- `skip_reason` 在响应中只有 `pending_delivery` 或 null。
- `error` 是 `{code,message}` 或 null；常见 code 包括 `target_session_not_found`、`target_session_failed`、`delivery_failed`、`session_unavailable`、`invalid_target`。

投递边界：

- 定时投递由服务进程执行；进程停机期间不会投递。
- `periodic` 的执行时间晚于计划超过 60 秒时，视为错过，不补跑，直接推进到下一个未来时间。
- 手动触发在响应后异步投递；如果进程在投递前退出，该记录可能停留在 `queued`。
- `existing_session` 目标在投递时失败：周期自动化可能被暂停并记录 `pause_reason`；一次性自动化把失败记录在 invocation 上。

## 5. 本地降级与能力边界

降级通过响应头 `x-platform-degraded` 暴露，值是逗号分隔的原因列表（可能带空格）。读取资源时也会带上已持久化的原因。

| 原因 | 场景 | 实际行为 |
| --- | --- | --- |
| `environment_type_mapped_to_local` | Session/Environment/Automation 使用未知托管类型，或 Session 使用 `local` | 在本地执行；Session 的 `environment` 报告 `self_hosted` 形状，`workspace_directory` 可能为空 |
| `environment_type_filter_mapped_to_local` | `GET /agents/environments?type=<未知值>` | 过滤条件改为 `local` |
| `packages_not_installed` | 环境请求包含 packages | 包不会安装 |
| `setup_commands_not_executed` | 环境请求包含 `setup_commands` | 命令不会执行，也不会保存 |
| `network_policy_not_enforced` | 环境网络策略不是 `disabled` | 策略不会执行 |
| `desktop_not_provisioned` | `desktop.enabled:true` | 不提供桌面 |
| `skills_not_installed` | 环境请求包含 skills | 只保存元数据（名称/描述，或 skill id/version），不安装 |
| `plugins_not_installed` | 环境请求包含 plugins | 只保存名称/描述，不安装 |
| `container_size_not_enforced` | 指定 `container_size` | 不限制资源规格 |
| `vaults_not_attached` | 环境创建带 `vault_ids` | 不注入凭据 |
| `subagent_history_unavailable` | 外部会话存储不可用 | 子 agent 历史列表为空页；单条 turn 为 503 |
| `computer_use` / `function` | Agent 包含这些工具 | 工具声明会被保存，但不执行 |

其他边界：

- **Provider**：`/v1` 没有 Provider 管理端点；Provider 通过同源管理接口 `/api/providers` 配置（不需要 `Authorization`，只接受回环 Host/同源 Origin），最小步骤见 §2。对于未指定 Provider 的 Agent，会话执行要求部署里恰好一条 Provider 记录（且有凭据），且 Store 可达；这类 Agent 在零条或多条时创建 Session 返回 400 `invalid_input`。运行时未就绪时返回 503 `engine_unavailable`。
- **会话环境**：Session 只接受托管类型，不接受已创建的 Environment ID。`workspace_directory` / `capability_directories` 不参与本地执行。
- **Vault 注入**：Session 的 `vault_ids` 只校验存在性并记录；本地运行时不读取或注入 credential。
- **输入内容**：消息只使用 `input_text` 的文本；其他内容类型（例如图片）不会被处理。文本为空或超过 32,768 字符返回 400。
- **待处理动作**：`required_actions` 中的 `function_call` 没有对应的输入事件类型，无法从本接口回应；`mcp_approval_request` 可以用 `agent.session.input.mcp_approval_request_result` 回应。
- **流选项**：`stream_options.include_obfuscation` 被接受但不产生填充事件。
- **环境文件**：内容保存在应用数据库中，不会物化为 workspace 文件；没有读取文件内容的端点。
- **Artifacts**：本部署不从托管环境发布 artifact，列表通常为空；只有已存在的记录才能读取或删除。
- **Traces**：本部署通常不发布 trace，列表为空。
- **Automations**：手动触发只保证记录为 queued；投递依赖进程继续运行。周期任务超过 60 秒的错过不补跑。
- **删除级联**：删除 Vault 删除其 credentials；删除 Automation 删除其 invocations；删除 Session 删除其事件日志。删除 Agent 在仍有 Session 引用时返回 409。`/v1` 没有 Environment 删除操作，环境及其文件记录会保留。

## 6. 排错顺序

1. **401**：确认每个请求都有 `Authorization: Bearer <非空值>`；`Basic` 或其他 scheme 也会 401。
2. **404**：确认路径带 `/v1` 前缀，且父资源 ID 属于当前部署；子资源还要属于父资源。
3. **400 `invalid_input`**：确认 `Content-Type: application/json`，没有未知字段，分页参数合法；看 `param` 指向的字段。
4. **409 `session_active`**：等当前 turn 结束，或先提交 `agent.session.input.cancel`，再删除会话。
5. **SSE 没有事件**：确认 `Content-Type` 是 `text/event-stream`、请求带 bearer、会话 ID 存在；事件是持久化的，重新连接会从头或从 `Last-Event-ID` 之后重放。
6. **输入 400**：确认消息里至少有一个 `input_text` 部分且文本非空；只有图片或其他类型会被视为空输入。
7. **会话不执行**：检查部署的 `/api/health` 是否为 `engine.status: "ready"`（`ok:true` 不代表运行时 ready），并通过 Providers 页面或 `GET /api/providers` 确认 Provider 与模型可用（未指定 Provider 的 Agent 应恰好一条且有凭据）；Store 不可达时也会停在这里。引擎端口被占用时健康检查同样失败但不点名端口，按 1.3 节检查 `PERI_ENGINE_PORT` 与其 +1 端口。`/v1` 的错误信封只返回稳定的 `code`；具体失败原因见会话或 turn 的 `error` 字段，内容取决于运行时。
8. **时间不对**：所有时间戳都是秒；不要按毫秒解析。
