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
- 本机 8080 若被占用，改 `PERI_SWARM_BIND` 与 SDK 的 `baseURL`。

构建与启动 Rust 侧（必须经补丁脚本，`Cargo.lock` 指向打过补丁的 `rmcp`）：

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm
./target/debug/peri-swarm
# 等价入口：./scripts/cargo-rmcp-patched.sh run -p peri-swarm
```

直接 `cargo run -p peri-swarm` 不会注入补丁配置，会在依赖解析阶段失败
（`cargo metadata --locked` 可复现）。

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
   cd npm-packages/peri-sandbox
   bun install
   PERI_SWARM_BUN_TOKEN=<与网关相同的值> \
   PERI_PATH=<仓库>/target/debug/peri \
   bun run dev:swarm
   ```

   默认 `127.0.0.1:8081`，沙箱根默认 `~/.peri-swarm/sandboxes`，空闲回收默认 30 分钟
   （`PERI_SWARM_IDLE_MS=0` 关闭）。详细变量见
   [peri-sandbox README](../npm-packages/peri-sandbox/README.md#本地-swarm-服务devswarm)。

网关可以先于 Bun 启动；在 Bun 就绪前创建的 `openai_hosted` 会话会得到
`502 upstream_unavailable`，不会降级到本机执行。

## 官方 SDK 示例

网关只暴露 `/v1` 下的五个 Agents API 方法，因此用官方 `openai` SDK 时设置
`baseURL` 指向本机。`OpenAI-Beta: agents=v1` 可省略。

```js
import crypto from 'node:crypto';
import OpenAI from 'openai';

const client = new OpenAI({
  apiKey: process.env.PERI_SWARM_API_KEY,
  baseURL: 'http://127.0.0.1:18080/v1',
});

// create：首轮同步完成；environment 省略等价于 {type:'none'}
const session = await client.beta.agents.sessions.create({
  agent: { model: 'stub-model', instructions: 'Answer briefly.' },
  input: 'hello',
});
const id = session.id; // 等于 logicalId

// get
const current = await client.beta.agents.sessions.retrieve(id);

// events POST：网关一次只收一条输入，必须带 inputId
await client.post(`/agents/sessions/${id}/events`, {
  body: {
    inputId: crypto.randomUUID(),
    kind: 'user',
    mode: 'turn',
    text: 'follow up',
  },
});

// events SSE：after 是 item seq；断线后用 lastEventId 续读
const stream = await client.beta.agents.sessions.events.stream(id, {
  after: current.lastItemSeq,
});
for await (const event of stream) {
  if (event?.state === 'completed' || event?.state === 'failed') break;
}

// items：响应字段是 items/hasMore/nextCursor（不是 data/has_more）
const page = await client.beta.agents.sessions.items.list(id, {
  order: 'asc',
  limit: 50,
});
console.log(page.body.items, page.body.hasMore, page.body.nextCursor);
```

与官方 Agents API 的形状差异（已记录，不做兼容层）：

- `sessions.events.create({events:[...]})` 的批量 body 会被 `400 unknown_field`
  拒绝；用同一 client 的 `post` 发送单条输入。
- items 的 `CursorPage` 自动翻页不适用，请读取原始 `body` 并用 `nextCursor`
  显式翻页；`AgentSession.status` 也不存在，状态字段是 `state`。
- `sessions.stream()` 依赖 `status`，因此用 `events.stream()`。

## 已知限制

- 模型侧没有内置真实供应商；本地验收使用确定性 stub。真实模型需要
  `PERI_SWARM_SETTINGS_JSON` 中的 provider/profile 配置。
- 只杀 Rust 宿主的 reattach 已实现且验证；未完成 turn（行状态 `running`）不会
  `session/resume`，返回 `502 session_resume_required`。
- 后台任务引用（MCP `resultType:"task"`）尚未进入空闲回收判定；长任务期间可能被
  回收，需要重新 ensure。
- 仅绑定 loopback + 单 Bearer；无生产 TLS、多租户与跨机调度。1000 Agent 规模、
  Linux SRT 与真实第三方 MCP 未验收。

## 测试

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
cd npm-packages/peri-sandbox && bun run build && node --test dist/tests/swarm/*.test.js
```

`peri-swarm` 的 `loom_store` 测试默认被 `ignored`，需要真实 Loom 时显式启用。
