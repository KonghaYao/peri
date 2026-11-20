# peri-swarm

本地单机 Agent 网关：HTTP 面 → Rivet actor → 进程内 Peri ACP Host → 远程 workspace
MCP（Bun 托管的沙箱）→ 两个 Loom 逻辑库。设计边界见
[Peri Swarm 本地开发宿主](../docs/design/peri-swarm.md)，验收证据见
[本地开发链路验收](../docs/verification/peri-swarm-local-dev.md)。

本文是本地操作手册，不是运行时权威：字段与错误码以 `src/` 与契约测试为准。

## 前置

- macOS（SRT 走 `sandbox-exec`）或带 `systemd-run --user` 的 Linux；非 root。
- Rust（仓库补丁脚本）、Bun >= 1.4、Node >= 22.12、Python 3（provision 脚本）。
- 一个可用的 Peri Loom 本地栈（loopback HTTP），预构建 `peri-loom-v1` 与
  `peri-platform`。
- 沙箱内运行的 Peri 二进制：`./scripts/cargo-rmcp-patched.sh build --locked -p
  peri-tui --bin peri`（产物 `<repo>/target/debug/peri`）。
- 本机 8080 若被占用，同时改 `PERI_SWARM_BIND` 和 SDK 的 `baseURL`；不要只改一处。

构建与启动 Rust 侧（必须经补丁脚本，`Cargo.lock` 指向打过补丁的 `rmcp`）：

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
./target/debug/peri-swarm
# 等价入口：./scripts/cargo-rmcp-patched.sh run -p peri-swarm
```

直接 `cargo run -p peri-swarm` 不会注入补丁配置，会在依赖解析阶段失败
（`cargo metadata --locked` 可复现）。

> 环境差异：本机若使用忽略 target 目录的 cargo fork（例如 huacnlee fork），会打印
> `warning: ignoring the configured target directory ... this cargo always uses ./target`，
> `CARGO_TARGET_DIR` 不生效，产物固定写入 checkout 的 `target/`。这是本机工具链行为，
> 不是网关缺陷；用上面的补丁脚本并直接引用 `<repo>/target/debug/...` 即可。

## 环境变量（网关）

必填项缺失都会让进程启动失败，不回退到本地 SQLite。

| 变量 | 必填 | 说明 |
| --- | --- | --- |
| `PERI_SWARM_API_KEY` | 是 | 部署 Bearer key；空值拒绝 |
| `PERI_SWARM_SESSION_STORE` | 是 | Peri 会话库 locator，如 `http://127.0.0.1:9497/db/local_default/demo/main/` |
| `PERI_SWARM_STATE_STORE` | 是 | Swarm 状态库 locator，如 `http://127.0.0.1:9497/db/local_default/swarm/main/`；必须与会话库不同 |
| `PERI_SWARM_STORE_TOKEN_ENV` | 是 | **变量名**（如 `PERI_SWARM_STORE_TOKEN`），不是 token 值；被命名变量必须非空 |
| `PERI_SWARM_MACHINE_ID` | 是 | 稳定机器 UUID |
| `PERI_SWARM_WORKSPACE_ROOT` | 是 | 绝对、规范化目录；macOS 的 `/tmp` 需写成 `/private/tmp` |
| `PERI_SWARM_BIND` | 否 | 默认 `127.0.0.1:8080`，只接受 loopback |
| `PERI_SWARM_STORE_ENGINE` | 否 | 默认 `turso` |
| `PERI_SWARM_MODEL` | 否 | 部署级默认模型 |
| `PERI_SWARM_SETTINGS_JSON` | 否 | 可信 settings 文档（JSON），提供 providers/profiles 与 `config.active_alias` |
| `PERI_SWARM_BUN_URL` | 否 | Bun 沙箱桥地址；同时设置 `PERI_SWARM_BUN_BASE_URL` 时后者优先 |
| `PERI_SWARM_BUN_TOKEN` | 否 | Bun 外层 Bearer 值；与 `PERI_SWARM_BUN_BEARER_ENV`（变量名）互斥 |
| `PERI_SWARM_SSE_HEARTBEAT_MS` | 否 | SSE 心跳，默认 15000 |

未配置 Bun 桥时，`environment.type=openai_hosted` 在创建会话时显式失败；
`environment.type=none` 不受影响。

## 启动顺序

1. **Loom 双库**：启动本地 Loom（示例监听 `127.0.0.1:9497`），`dev.py` 会种出
   Peri 会话库 `local_default/demo/main`。Loom 只支持 loopback HTTP
   （`PERI_HRANA_EXPERIMENTAL=1`）。

   `dev.py --help` **不列出**二进制路径参数；它从环境变量读取
   `PERI_RUST_BINARY`（默认 `prototypes/v1/target/debug/peri-loom-v1`）和
   `PERI_PLATFORM_BINARY`（默认 platform 目录下的 `peri-platform`）。
   `--no-build` 不会替你构建；没有源码构建产物时用预构建二进制：

   ```bash
   cd <peri-loom>/prototypes/v1/platform
   PERI_HRANA_EXPERIMENTAL=1 \
   PERI_RUST_BINARY=/absolute/path/to/peri-loom-v1 \
   PERI_PLATFORM_BINARY=/absolute/path/to/peri-platform \
   python3 dev.py --no-build --listen 127.0.0.1:9497 \
     --run-dir /absolute/path/to/loom-run
   ```

   就绪日志是 `{"ready": true, "service": "local-project", ...}`，随后 9497 进入
   LISTEN。用普通 `&` 从一次性 shell 启动的进程会随 shell 退出；请放到受管后台任务
   或长驻 shell。失败后会留下 `launcher-stop.json`（`outcome: unclean`），用新的
   `--run-dir` 重试。

2. **provision Swarm 状态库**：

   ```bash
   python3 peri-swarm/scripts/provision_state_db.py \
     --connection-file /path/to/loom/run/connection.json
   ```

   脚本创建 `local_default/swarm/main` 并探测 Hrana；同一 `request_id` 幂等，不同
   `request_id` 指向已存在身份会显式失败。密钥只从 `connection.json` 读入请求头，
   不打印、不落盘。

3. **Rivet**：本地 engine 为 Rivet 3.1.1（`~/.rivetkit/engine/v3`），endpoint 固定
   `http://127.0.0.1:6420`。首次运行设置 `RIVETKIT_ENGINE_AUTO_DOWNLOAD=1`，网关会按需
   拉起；预装 engine 时无需额外步骤。
4. **网关**：按上表导出环境变量后运行 `./target/debug/peri-swarm`。`/healthz` 是不鉴权
   的常量探针；启动日志不带 token。
5. **Bun 沙箱服务**（`openai_hosted` 会话必需）：

   ```bash
   ./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
   cd npm-packages/peri-sandbox
   bun install

   # 推荐：把 periPath 与 SHA-256 固化进 settings（目录必须为 0700）
   PERI_PATH=<repo>/target/debug/peri \
     bun run src/cli.ts init --settings /absolute/path/to/settings.json

   PERI_SWARM_BUN_TOKEN=<与网关相同的值> \
   PERI_PATH=<repo>/target/debug/peri \
   PERI_SANDBOX_SETTINGS=/absolute/path/to/settings.json \
   PERI_SWARM_SANDBOXES_ROOT=/absolute/path/to/sandboxes \
   bun run dev:swarm
   ```

   默认 `127.0.0.1:8081`，沙箱根默认 `~/.peri-swarm/sandboxes`，空闲回收默认 30 分钟
   （`PERI_SWARM_IDLE_MS=0` 关闭）。有 `PERI_SANDBOX_SETTINGS` 时，`periPath` 与
   `periSha256` 以文件为准；`PERI_PATH` 只用于没有 settings 文件的默认配置。手写
   settings 时 `periSha256`、profile 的资源字段与 `sandboxesRoot` 的 0700 权限都是
   必填/必检项，错误信息会带字段名。SRT policy 会自动放行 `periPath` 的父目录，
   因此默认 `<repo>/target/debug/peri` 不需要额外改 policy。详细变量见
   [peri-sandbox README](../npm-packages/peri-sandbox/README.md#本地-swarm-服务devswarm)。

网关可以先于 Bun 启动；在 Bun 就绪前创建的 `openai_hosted` 会话会得到
`502 upstream_unavailable`，不会降级到本机执行。

## 官方 SDK 示例

网关只暴露 `/v1` 下的五个 Agents API 方法，因此用官方 `openai` SDK 时设置
`baseURL` 指向本机实际 bind 的地址。`OpenAI-Beta: agents=v1` 可省略。

```js
import crypto from 'node:crypto';
import OpenAI from 'openai';

const baseURL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:8080/v1';
const client = new OpenAI({
  apiKey: process.env.PERI_SWARM_API_KEY,
  baseURL,
});

// create：首轮同步完成；environment 省略等价于 {type:'none'}
const session = await client.beta.agents.sessions.create({
  agent: { model: 'stub-model', instructions: 'Answer briefly.' },
  input: 'hello',
});
const id = session.id; // 等于 logicalId

// get：hosted 会话的 sandbox 投影来自 swarm_bindings
const current = await client.beta.agents.sessions.retrieve(id);
console.log(current.state, current.environment);
if (current.sandbox) {
  // { logicalEnvId, sandboxId, runId, state, workspacePath }
  // state 为 null：网关不跟踪 Bun 的实时生命周期，见下方说明。
  console.log(current.sandbox.workspacePath);
}

// 历史 items：显式翻页。nextCursor 是 seq 数字，请求参数名是 after。
// SDK 的 CursorPage 自动翻页不适用；TS 中 body 是 protected，需要 cast。
let after = 0;
for (;;) {
  const page = await client.beta.agents.sessions.items.list(id, {
    order: 'asc',
    after,
    limit: 50,
  });
  const body = /** @type {any} */ (page).body;
  for (const item of body.items) {
    // item 是事件级帧：delta 与 complete 共享 itemId/id，按 payload.messageId 聚合；
    // 最终文本以 payload.complete === true 的那一帧为准。
  }
  if (!body.hasMore) break;
  after = body.nextCursor;
}

// 回放首轮：必须显式三个实参。
// 两实参时 openai SDK 会把 { after } 当成 RequestOptions，after 不会进入 query。
const replay = await client.beta.agents.sessions.events.stream(
  id,
  { after: 0 },
  {},
);
for await (const event of replay) {
  // 回放只有 item 帧，没有 turn 控制帧；忽略心跳（只有 lastItemSeq）。
  if (typeof event?.seq !== 'number') continue;
  if (event?.payload?.complete === true) break; // 回放终止条件
}

// 追问：POST /events 是同步的——202 返回时本轮已进入终态，
// lastItemSeq 已包含本轮所有 item。不要把它当异步排队再轮询第二次。
const ack = await client.post(`/agents/sessions/${id}/events`, {
  body: {
    inputId: crypto.randomUUID(),
    kind: 'user',
    mode: 'turn',
    text: 'follow up',
  },
});
console.log(ack.lastItemSeq);

// 实时续读：从 ack.lastItemSeq 之后开始，同样用三个实参。
const live = await client.beta.agents.sessions.events.stream(
  id,
  { after: ack.lastItemSeq },
  {},
);
for await (const event of live) {
  if (event?.state === 'completed' || event?.state === 'failed') break; // turn 控制帧
  if (typeof event?.seq !== 'number') continue; // 心跳 { lastItemSeq }
  if (event?.payload?.complete === true) console.log('assistant message complete');
}
```

### SSE 终止条件与游标

| 场景 | 帧类型 | 终止条件 |
| --- | --- | --- |
| 回放（`after` 小于已有 seq） | 只有 `item` | 目标 assistant message 的 `payload.complete === true`；没有 `turn` 帧，`event.state` 不存在 |
| 实时 | `item` + `turn`（可能有 `gap`） | `event.state === 'completed' \| 'failed'` |
| 心跳 | `{ "lastItemSeq": N }` | 无 `seq`、无 `state`；忽略即可，不推进游标 |

`after` 是「只补 `seq > after` 的 item」；`after=0`（或省略查询参数）回放全部历史。
`Last-Event-ID` 头与 `after` 等价（都按 seq 续读），区别只在传输方式：官方 SDK 没
暴露该头，所以 SDK 用户必须用 `events.stream(id, { after }, {})`；直接用 HTTP 时可以写：

```js
const response = await fetch(
  `${baseURL}/agents/sessions/${id}/events`,
  {
    headers: {
      Authorization: `Bearer ${process.env.PERI_SWARM_API_KEY}`,
      Accept: 'text/event-stream',
      'Last-Event-ID': String(current.lastItemSeq),
    },
  },
);
// 响应是 SSE 字节流；每个 item 帧的 `id:` 字段就是 seq。
// turn 与 gap 帧不持久化、不带 `id:`，不会推进 Last-Event-ID。
```

### items 与官方 item 形状的差异

- items 是**事件日志**，不是高层 message/run item：`kind` 为 `message`、`tool_call`、
  `tool_update`、`tool_result` 等，delta 帧与 complete 帧都各占一行。
- 每帧现在同时有 `id` 与 `itemId`（同值，`itim_...`）；同一消息的 delta/complete 帧
  共享这个 id，因此不能用 `id` 去重成一条消息。按 `payload.messageId` 聚合，最后以
  `payload.complete === true` 帧的 `payload.text` 为准。
- `nextCursor` 是这页最后一条的 seq；下一页把它传给 `after`（也接受 `afterSeq`）。
  传 `cursor` 会得到 `400 invalid_query`。`order=desc` 时 `after` 表示「更旧于该 seq」。
- SDK `CursorPage` 要求 item 有 `id` 并自动翻页；这里的 item 是事件帧，语义不同，
  请显式读 `body.items`。TS 中 `page.body` 声明为 `protected`，运行时可用；需要类型时
  自行 cast（例如 `(page as any).body` 或声明局部接口）。

### hosted 会话的 sandbox / workspace

- `environment` 仍是 `{ type: 'openai_hosted', id: 'env_...' }`；`logicalEnvId` 就是 Bun
  的逻辑环境 id。
- `sandbox` 字段在 hosted 会话上非 null：`sandboxId`、`runId`（懒预留时为 null）、
  `workspacePath` 来自 `swarm_bindings`。`state` 为 null，因为网关不跟踪 Bun 的实时
  SRT 状态；需要 `ready`/`stopped` 等实时状态时调用 Bun 的
  `GET /v1/sandboxes/{environment.id}`（外层 Bearer）。
- 会话的 `cwd` 是网关会话工作目录，不是工具执行目录；工具在
  `sandbox.workspacePath` 下执行。文件内容不在 items 的 `tool_result.output` 里
  （当前只有退出摘要），需要读 workspace 或 Bun 的状态接口。

与官方 Agents API 的形状差异（已记录，不做兼容层）：

- `sessions.events.create({events:[...]})` 的批量 body 会被 `400 unknown_field`
  拒绝；用同一 client 的 `post` 发送单条输入。
- `AgentSession.status` 不存在，状态字段是 `state`；`sessions.stream()` 依赖
  `status`，因此用 `events.stream()`。
- `POST /events` 目前是**同步**语义：网关在返回 HTTP 202 前等待本轮进入终态
  （`mode=turn`），回执中的 `lastItemSeq` 已经包含本轮 item。它不是「已排队、稍后
  执行」的异步受理。

## 已知限制

- 模型侧没有内置真实供应商；本地验收使用确定性 stub。真实模型需要
  `PERI_SWARM_SETTINGS_JSON` 中的 provider/profile 配置。
- 只杀 Rust 宿主的 reattach 已实现且验证；未完成 turn（行状态 `running`）不会
  `session/resume`，返回 `502 session_resume_required`。
- 后台任务引用（MCP `resultType:"task"`）尚未进入空闲回收判定；长任务期间可能被
  回收，需要重新 ensure。
- `sandbox.state` 在会话投影中固定为 `null`：网关只持久化绑定，不跟踪 Bun 的实时
  启停；实时状态以 Bun `GET /v1/sandboxes/{id}` 为准。
- 仅绑定 loopback + 单 Bearer；无生产 TLS、多租户与跨机调度。1000 Agent 规模、
  Linux SRT 与真实第三方 MCP 未验收。

## 测试

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
cd npm-packages/peri-sandbox && bun run build && node --test dist/tests/swarm/*.test.js
```

`peri-swarm` 的 `loom_store` 测试默认被 `ignored`，需要真实 Loom 时显式启用。

端到端脚本使用 `peri-swarm/scripts/local_model_stub.mjs`。设置
`PERI_STUB_TOOL_COMMAND` 时，stub 每个用户轮次最多发出一次 `Bash` 工具调用（收到
tool 结果后不再重复），同一进程可连续服务多个轮次；`GET /__stats` 查看计数，
`POST /__reset` 清零。需要按输入选择命令时用 `[[tool:<name>]]` + `PERI_STUB_COMMANDS`。
这些是 fixture 行为，不是网关或官方 API 的语义。
