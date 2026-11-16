# 自定义 transport runtime

本文写给需要自己提供 ACP 后端的调用方：`ChatConfig.runtime` 的 `kind: "transport"` 分支、SDK 实际发出的请求/通知形状，以及 `chat.close()` 与 `session/close` 的前置关系。所有方法名与字段名以当前实现为准。

包内自带一个不依赖 ACP、进程或 LLM 的参考实现 `createEchoChatRuntime()`，可以先用它跑通 actor 闭环，再接真实运行时。

## 1. runtime 在配置里的形状

`ChatConfig.runtime` 有两个分支（`@peri-code/sdk` 的 `ChatRuntime`）：

```ts
type ChatRuntime =
  | {
      kind: 'wasm';
      moduleUrl?: string | URL;
      moduleFactory?: PeriWasmModuleFactory;
      env?: Readonly<Record<string, string>>;
      onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
    }
  | {
      kind: 'transport';
      factory: (path: string, options?: ChatTransportFactoryOptions) => Transport | Promise<Transport>;
    };

interface ChatTransportFactoryOptions {
  signal?: AbortSignal;
  chatId?: string;
  agentConfig?: unknown;
}
```

- `factory` **每次激活调用一次**：`create_new_session`、`change_session` 都会启动新的 transport。同一次激活内复用同一个 `Transport` 实例。
- `path` 是本次激活的会话 cwd（由 `workspace.runtimePath` + `sessionPath` 派生）；`options.chatId` 是 Chat 句柄 id，`options.agentConfig` 是冻结后的 agent 配置 JSON。
- 工厂返回的对象实现 SDK 的 `Transport`：

```ts
interface Transport {
  request<T = unknown>(method: string, params?: unknown): Promise<T>;
  sendRequest<T = unknown>(method: string, params?: unknown): Promise<{ response: Promise<T> }>;
  notify(method: string, params?: unknown): Promise<void>;
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void;
  events(): AsyncIterable<JsonRpcNotification>;
  setRequestHandler(handler: ReverseRequestHandler): void;
  close(): Promise<void>;
}
```

`request` 抛出错误即协议失败；`subscribe` 负责把通知推给 SDK（`events()` 可另给一个异步迭代消费者，SDK 自身只用 `subscribe`）。`setRequestHandler` 用于宿主向客户端发反向请求（`mcp/message`、权限、elicitation）；不实现反向请求时可以空实现。

## 2. 最小实现：createEchoChatRuntime

```ts
import type { ChatConfig } from '@peri-code/sdk';
import { createEchoChatRuntime } from '@peri-code/rivet/rivet';

const chatConfig: ChatConfig = { runtime: createEchoChatRuntime() };
```

导出内容：

```ts
function createEchoChatRuntime(): Extract<ChatRuntime, { kind: 'transport' }>;
const ECHO_REPLY_PREFIX: 'echo: ';
type EchoChatRuntime = Extract<ChatRuntime, { kind: 'transport' }>;
```

行为：

- 把每条用户输入回显为 assistant 文本 `` `${ECHO_REPLY_PREFIX}${text}` ``（默认即 `echo: <text>`）。
- 每个输入产生一个 turn，多个输入按 enqueue 顺序串行执行。
- 每个激活一个 transport；不跨激活持久化会话或历史。`session/load` 接受任意 id 并建立空队列，不回放任何通知。
- `close()` 只关闭激活；再次 `open` 得到空 binding，facts 为空。刷新后历史仍在只能靠宿主持久化投影，或换真实 runtime。
- 只有一个内存队列；`session/close` 成功返回，因此 `chat.close()` 不会得到 `close_incomplete`。

完整 actor 装配与 README 的 actor 装配示例一致，只替换 runtime：

```ts
import type { ChatConfig, KvStorage } from '@peri-code/sdk';
import {
  createEchoChatRuntime,
  createNoopChatDatabase,
  createPeriChatActor,
} from '@peri-code/rivet/rivet';
import { setup } from 'rivetkit';

const records = new Map<string, unknown>();
const kv: KvStorage = {
  getItem: async (key) => records.get(key) ?? null,
  setItem: async (key, value) => {
    records.set(key, value);
  },
  removeItem: async (key) => {
    records.delete(key);
  },
};

const chatConfig: ChatConfig = { runtime: createEchoChatRuntime() };

export const registry = setup({
  use: {
    chat: createPeriChatActor({
      openDatabase: createNoopChatDatabase,
      ownership: kv,
      machineId: '00000000-0000-4000-8000-000000000001',
      chatConfig,
    }),
  },
  startEngine: true,
});
```

客户端调用顺序：`open(target)` → `execute({ type: 'create_new_session', requestId })` → `execute({ type: 'send', requestId, payload: { text } })` → `facts()` → `close()`。`send` 的回执只表示入队被接受；assistant 文本通过 `facts()` 的 `notice` 事实到达。

## 3. 请求清单

SDK 在 open → `create_new_session` → `send` → `close` 流程中会发出下列请求。`params` 的字段名由 SDK 侧代码固定。表格每行先列 SDK 发送的字段，再注明 echo 实际读取的部分；未注明的字段 echo 忽略。实现真实运行时不能只照 echo 的读取范围：`_meta["peri.instructions"]`、`originalDraft` 等同样是 SDK 约定的一部分，需要按 ACP 语义处理。

| method | params（SDK 发送；echo 读取） | result（echo 返回） | 何时调用 |
| --- | --- | --- | --- |
| `initialize` | `protocolVersion`、`clientCapabilities`、`clientInfo?`；echo 不读取 `params` | `{ protocolVersion: 1, agentCapabilities: {}, authMethods: [] }` | 每次激活的第一个请求 |
| `session/new` | `cwd`、`mcpServers`、`_meta?`（`_meta["peri.instructions"]`）；echo 只读 `cwd` | `{ sessionId, configOptions: [] }` | `create_new_session` |
| `session/load` | `sessionId`、`cwd`、`mcpServers`、`loadGeneration`（SDK 必发）；echo 只读 `sessionId`，可选读取 `loadGeneration` | `{ sessionId, snapshotToken: null, nextCursor: null, _meta?: { periLoadGeneration } }` | `change_session`（显式加载已有 Session；`open` 本身不调用） |
| `session/input/snapshot` | `sessionId`；echo 只读 `sessionId` | `{ generation, revision, items: [{ inputId, state }] }` | 每次激活的启动/绑定阶段一次；显式调用 `refreshInputQueue()` 时 |
| `session/input/enqueue` | `sessionId`、`generation`、`commandId`、`inputId`、`content`、`originalDraft`；echo 读 `sessionId`、`generation`、`inputId`、`content`，`commandId` 存在时用于幂等，忽略 `originalDraft` | `{ results: [{ inputId, state }], snapshot }` | `send` |
| `session/input/dispatch` | `sessionId`、`generation`、`commandId`、`inputIds`；echo 读 `sessionId`、`generation`、`inputIds`，`commandId` 存在时用于幂等 | `{ results: [{ inputId, state }], snapshot }` | `force_send` |
| `session/input/takeback` | `sessionId`、`generation`、`commandId`、`inputId`；echo 读 `sessionId`、`generation`、`inputId`，`commandId` 存在时用于幂等 | `{ results: [{ inputId, state }], snapshot }` | `take_back` |
| `session/rename` | `sessionId`、`title`；echo 只读 `sessionId`、`title` | `{}` | `create_new_session` 带 `title` 时 |
| `session/close` | `sessionId`；echo 只读 `sessionId` | `{}` | `chat.close()` |

约束与细节：

- `generation` 必须等于 `session/input/snapshot` 给出的值。代次不一致时抛 `RpcError`，`code: -32602`，`data: { rejected: true, snapshot }`；SDK 会识别为 `SessionQueueStaleError` 并阻断该激活，不会自动重放用户内容。
- `results[].state` 取值 `queued` / `dispatching` / `claimed` / `delivered` / `withdrawn`；`unknown` 表示命令未被接受。判定在 SDK 侧：`enqueue` 和 `dispatch` 的回执缺少该 `inputId` 或状态为 `unknown`、`withdrawn` 时抛 `Input not accepted`；`takeback` 的回执缺少该 `inputId` 或状态为 `unknown` 时抛 `Input not taken back`。echo 的 `enqueue` 不返回 `unknown`；它的 `dispatch` 对未跟踪的 `inputId`、`takeback` 对已投递或未知输入返回 `unknown`。
- `commandId` 用于幂等：echo 按 `commandId` 缓存最近 1024 条回执，重复请求返回同一回执，不重复执行。
- `session/rename` 只在 `create_new_session` 的 `payload.title` 非空时调用；不实现会导致创建失败。
- `session/load` 的响应若回带 `_meta.periLoadGeneration`，非空值必须与请求的 `loadGeneration` 一致，否则 SDK 拒绝该响应；省略该字段时 SDK 不校验。没有 `snapshotToken` 时不协商历史分页，`load_earlier_history` 会以 `not_negotiated` 失败且不发送请求。
- `notify("session/cancel", { sessionId })` 是通知而非请求。echo 会终止已发出 `user_input_run_started`、但尚未跑到终态的 turn，并发出 `stopReason: "cancelled"` 的 `peri/agent_event_done`。
- 未实现的方法抛 `RpcError(-32601, ...)`；不要静默返回 `undefined`，否则调用方会把失败当成成功。

## 4. 通知清单与顺序

每个 echo turn 固定五条通知，顺序如下。`sessionId` 在每条 notification 的 `params` 顶层。所有通知的公共外层都是 `{ "jsonrpc": "2.0", "method": <name>, "params": <params> }`；下文的第 2–5 段为了突出差异省略了重复的 `jsonrpc` 键。

1. `peri/agent_event`：`user_input_run_started`（在 `session/input/enqueue` 处理期间、响应返回前发出）

```json
{
  "jsonrpc": "2.0",
  "method": "peri/agent_event",
  "params": {
    "sessionId": "echo_session_...",
    "event_json": "{\"type\":\"user_input_run_started\",\"value\":{\"generation\":\"echo_generation_...\",\"request_id\":\"echo_request_1\"}}"
  }
}
```

2. `session/update`：`user_message_chunk`（`setTimeout(0)` 后）

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "echo_session_...",
    "update": {
      "sessionUpdate": "user_message_chunk",
      "messageId": "<inputId>",
      "content": { "type": "text", "text": "hello" }
    }
  }
}
```

3. `peri/agent_event`：`user_input_delivered`

```json
{
  "method": "peri/agent_event",
  "params": {
    "sessionId": "echo_session_...",
    "event_json": "{\"type\":\"user_input_delivered\",\"value\":{\"generation\":\"echo_generation_...\",\"input_id\":\"<inputId>\"}}"
  }
}
```

4. `session/update`：`agent_message_chunk`

```json
{
  "method": "session/update",
  "params": {
    "sessionId": "echo_session_...",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "messageId": "echo_reply_echo_request_1",
      "content": { "type": "text", "text": "echo: hello" }
    }
  }
}
```

5. `peri/agent_event_done`

```json
{
  "method": "peri/agent_event_done",
  "params": { "sessionId": "echo_session_...", "requestId": "echo_request_1", "stopReason": "end_turn" }
}
```

要点：

- `event_json` 是**字符串**，内容是 `{ "type": ..., "value": ... }`。SDK 只解析这一层；`value` 里的字段按事件类型读取。
- `request_id` / `requestId` 必须成对：SDK 用 `user_input_run_started` 的 `request_id` 作为当前 turn 身份，`peri/agent_event_done` 的 `requestId` 不匹配时不会结束 turn。
- `user_input_delivered` 决定 `send` 回执的投递完成；`input_id` 与 `generation` 必须与 enqueue 时一致，否则该输入不会被标记为已投递。
- `user_message_chunk` 的 `messageId` 使用 `inputId`。SDK 投影会把该 chunk 与 `input-delivered` 事实合并为同一个用户条目；如果两者文本都非空又使用了不同身份，用户文本会重复出现。
- `session/update` 必须带 `update.sessionUpdate` 和 `content.type = "text"`、`content.text`；assistant 文本只有在存在可写 turn 时才被投影，因此 `agent_message_chunk` 必须晚于开启 turn 的事件。
- 队列快照通过 `session/input/*` 回执和绑定阶段同步；如果运行侧另有队列变化，可发 `peri/agent_event` 的 `user_input_queue_changed`（`value.snapshot` 为新快照），echo 未使用。
- `peri/unstable_event`、`tool_call`、`agent_thought_chunk` 等扩展不实现时不要伪造；缺失只会让对应展示为空。

## 5. session/close 与 close_incomplete

`chat.close()` 的关闭顺序是：`ChatService.close()` → 当前激活的 `port.close()` → `PeriInstance.closeOnce()` → `Session.close()` → `session/close` → `Transport.close()`。

因此：

- **只要本次激活已经建立过 Session（`create_new_session` 成功），transport 就必须实现 `session/close`。** 缺失或拒绝会让关闭无法确认。
- `session/close` 返回成功即视为 Session 已关闭；SDK 随后调用 `transport.close()`，两者都成功才算本次激活关闭完成。
- 运行侧暂时无法关闭时，可以抛 `RpcError(-32010, "...")`。SDK 会在 `lifecycle.closeDrainTimeoutMs`（默认 `3000` ms）内每 50 ms 重放同一个 `session/close`；超时后按未完成处理。
- `session/close` 抛其他错误（包括普通 `Error`）不会重试，SDK 立即判定关闭未确认，并标记该激活 `needsReload`。
- 关闭未确认时，`chat.close()` 抛 `ChatCloseIncompleteError`；Rivet actor 的 `close` 动作把它映射为 `UserError`：`code: "close_incomplete"`，`metadata: { incomplete: true, errors: [...] }`。actor 会先清空自己持有的句柄，SDK 侧保留 ownership 与未确认激活；继续使用需要重新 `open`。
- 从未建立 Session 时，SDK 走 `cleanupStartup`，不会请求 `session/close`。

`activationCloseTimeoutMs`（默认 `5000` ms）是包住整个 `port.close()` 的上界；`closeDrainTimeoutMs` 只约束 `session/close` 的重放。预算按字段覆盖：

```ts
const chatConfig: ChatConfig = {
  runtime: createEchoChatRuntime(),
  lifecycle: { closeDrainTimeoutMs: 1_000, activationCloseTimeoutMs: 2_000 },
};
```

## 6. 换成真实 runtime

保留 actor 装配，只替换 `chatConfig.runtime`：

```ts
const chatConfig: ChatConfig = {
  runtime: {
    kind: 'transport',
    factory: (path, options) => createMyAcpTransport(path, options),
  },
};
```

`createMyAcpTransport` 需要：

1. 连接真实 ACP 后端（例如 stdio JSON-RPC 进程），把 `request` / `notify` 映射成 JSON-RPC 请求与通知，把收到的 notification 交给所有 `subscribe` 监听者。
2. 原样实现本文第 3 节的请求语义（尤其是 `initialize`、`session/new`、`session/load`、`session/input/*`、`session/close`）与第 4 节的通知顺序。
3. 把 `signal` 与 `close()` 接到进程终止/连接释放上；关闭失败时按 §5 区分可重试的 `-32010` 与不可重试的普通错误。
4. 若要支持 MCP 反向请求或权限交互，实现 `setRequestHandler` 并在收到后端请求时调用它。

也可以直接用 SDK 自带的 wasm 运行时：`{ kind: "wasm" }`。echo runtime 与真实后端的行为差异：不回放历史、不写 transcript、不产生 tool/thinking 事件、不读取 `instructions`/`mcpServers`（`session/new` 对 `cwd` 仅做非空校验，`session/load` 不读取 `cwd`），单个输入只产生一轮纯文本，`session/load` 接受任意 id 且不回放。
