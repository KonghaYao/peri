# Peri Swarm 本地端到端文本会话验收（S9a1）

- 状态：验收记录（一次真实本地运行）
- 日期：2026-11-20
- 工作树：`/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- 基线：`89b786a46c6034d24e6a77192218bbea552d041d`（S1..S8b）
- 范围：两库（Peri 会话库 + Swarm 状态库）真实 Loom、真实 Rivet engine、真实
  `peri-swarm` 进程；模型侧为本地确定性 stub；只覆盖 `environment.type=none` 的
  文本轮次。沙箱、工具、故障恢复不在本记录内（S9a2/S9a3）。
- 复现脚本（本阶段新增，已入库）：
  `peri-swarm/scripts/local_model_stub.mjs`、`peri-swarm/scripts/sdk_sessions_e2e.mjs`。
  运行产物、连接文件和日志在 `/tmp/peri-swarm-s9/`（不入库）。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 官方 `openai@7.62.0` 暴露 Agents API | 通过 | §3：`client.beta.agents.sessions.{create,retrieve,events,items}` 均存在 |
| create（`environment.type=none`、`agent.model=stub-model`、instructions、初始 input） | 通过 | §4.1：`agent.session`、`created=true`、`state=idle`、`lastItemSeq=5` |
| get | 通过 | §4.1：同一 `id`/`rootSessionId`，模型与环境回读一致 |
| events POST（追问） | 通过（见 §8 的 SDK 形状差异） | §4.2：官方高层调用被 400 拒绝并记录；同一官方 client 的低层 `post` 得到 `202 accepted` |
| events GET（SSE） | 通过 | §4.3：`turn started` → 4 个文本 delta（seq 6..9）→ `complete`（seq 10）→ `turn completed` |
| items GET（`order/after/limit/has_more`） | 通过 | §5：asc/desc 各 5 页、limit=2、无重复无遗漏、`hasMore` 在末页为 false |
| 同一会话 | 通过 | 所有调用使用 create 返回的 `id`；`items.logicalId` 与之一致；`rootSessionId` 未变 |
| 文本回答落 items | 通过 | `message:complete` 的 `payload.text` 与 stub 回复逐字相等 |
| SSE 增量与完成文本 | 通过 | 4 个 delta 的 `payload.text` 拼接等于 complete 文本，且共享同一 `itemId` |
| 两库隔离 | 通过 | §6：会话库 `peri_store_meta` v15、无 `swarm%` 对象；状态库 `swarm_meta` v1、4 表 + 1 索引 |
| 脚本可重复 | 通过 | 同一脚本连续 2 次运行（run5/run6）均 122 条断言通过，stub 文本逐字相同 |
| teardown 无残留 | 通过 | §10：18080/9496/9497/6420 无监听，无本任务进程；8080 仍为运行前的 OrbStack |

## 2. 组件与版本

| 组件 | 版本/标识 |
| --- | --- |
| Peri 工作树 HEAD | `89b786a46c6034d24e6a77192218bbea552d041d` |
| `peri-swarm` 二进制 | `target/debug/peri-swarm`，由 `./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm` 构建 |
| Peri Loom | `/Users/konghayao/code/ai/peri-loom` @ `12dd01a15985634396dcdc10a49212e38c16702b` |
| Loom 二进制 | `/tmp/peri-env-probe/loom/peri-loom-v1`、`/tmp/peri-env-probe/loom/peri-platform`（预构建） |
| Rivet engine | `~/.rivetkit/engine/v3/3.1.1/rivet-engine-aarch64-apple-darwin`，`--version` → `Rivet 3.1.1` |
| 官方 SDK | `openai@7.62.0`（`npm install openai@7.62.0`；版本由 `require.resolve('openai')` 旁的 `package.json` 读出） |
| 模型 | 本地 stub（`stub-model`），不访问外网 |

Loom 连接文件公开字段（`connection.json`，key 未打印）：

```json
{"endpoint":"http://127.0.0.1:9497","database":{"namespace":{"tenant":"local_default","database":"demo"},"branch":"main"},"ca_file":null}
```

## 3. 官方 SDK 的 Agents API 资源路径

在 `/tmp/peri-swarm-s9/sdk/node_modules/openai` 实际检查（不是按记忆推断）：

| 方法 | SDK 资源 | 实际请求 |
| --- | --- | --- |
| create | `client.beta.agents.sessions.create(body)` | `POST /agents/sessions`，自动带 `OpenAI-Beta: agents=v1` |
| get | `client.beta.agents.sessions.retrieve(id)` | `GET /agents/sessions/{id}` |
| events POST | `client.beta.agents.sessions.events.create(id, {events})` | `POST /agents/sessions/{id}/events` |
| events GET | `client.beta.agents.sessions.events.stream(id, query)` | `GET /agents/sessions/{id}/events`，`stream: true` |
| items GET | `client.beta.agents.sessions.items.list(id, {after,limit,order})` | `GET /agents/sessions/{id}/items?after=&limit=&order=`，解析为 `CursorPage` |

依据：`resources/beta/agents/sessions/sessions.js`（`post('/agents/sessions')`、
`get('/agents/sessions/${sessionID}')`）、`events.js`（`post`/`get .../events`）、
`items.js`（`getAPIList(.../items, CursorPage)`，query 键为 `after`/`limit`/`order`）。
同一层还暴露 `sessions.stream()`，但 `lib/agents/agent-session-stream.js` 在开始前读取
`session.status !== 'idle'`；Swarm 契约用 `state` 表达会话状态，因此本阶段使用
`events.stream()`，没有伪造 `sessions.stream()`。

`baseURL` 为 `http://127.0.0.1:18080/v1`，SDK 在每条请求上带 `Authorization: Bearer …`
与 `OpenAI-Beta: agents=v1`；网关的 Bearer 门禁实测：无 token `401`，正确部署 key `200`，
`OpenAI-Beta: agents=v0` `400`。

## 4. 五个方法的真实输出

以下取自最终脚本的 run6（`/tmp/peri-swarm-s9/logs/e2e-run6.json`）；run5 结论相同。

### 4.1 create / get

```text
session.id            = agent-01a1f9ab-eab6-76b3-9721-cd47ce27c6c1
session.logicalId     = agent-01a1f9ab-eab6-76b3-9721-cd47ce27c6c1
session.rootSessionId = 01a1f9ab-eac4-7792-81eb-2cdf7ef105b6
object=agent.session  created=true  state=idle  lastItemSeq=5
environment={"type":"none","id":"agent-01a1f9ab-eab6-…"}
agent.model=stub-model
cwd=/private/tmp/peri-swarm-s9/workspace/agent-01a1f9ab-eab6-…
get: id/rootSessionId 相同，state=idle，lastItemSeq=5
```

初始 input 的回答已落库：`message:complete`（seq 5），文本
`stub-echo:17496866c226#1`，其前有 4 个 delta（seq 1..4，同一 `itemId`）。

### 4.2 events POST（追问）

| 调用 | 结果 |
| --- | --- |
| `sessions.events.create(id, {events:[…]})` | `400`，`code=unknown_field`，`events: unknown field \`events\`, expected one of \`inputId\`, \`kind\`, \`mode\`, \`text\`, \`attachments\`, \`pluginSelections\`, \`originalDraft\`` |
| 官方 client 低层 `client.post('/agents/sessions/{id}/events', {body})` | `202`，`{inputId:"70f8664b-…", accepted:true, disposition:"inserted", mode:"turn", lastItemSeq:10}` |

两条都是官方 SDK 发出的真实 HTTP 请求；第二条是 `openai@7.62.0` 暴露的通用
`client.post`，用于网关的一次一事件 body。高层调用没有被重试或伪造成成功，原因见 §8。

### 4.3 events GET（SSE，先订阅后追问）

时序：`events.stream` 打开 SSE（`sseOpenMs=27`）→ 201 ms 后提交追问 → 读取到：

```text
turn:    {"state":"started","requestId":"…"}
item 6:  message delta "stub-e"        (itemId itim_01a1f9ab-fcb9-…)
item 7:  message delta "cho:59"
item 8:  message delta "acd9f1"
item 9:  message delta "2e99#2"
item 10: message complete "stub-echo:59acd9f12e99#2"
turn:    {"state":"completed","requestId":"…","stopReason":"end_turn"}
```

断言：turn 只有一对 `started`/`completed` 且 `requestId` 相同；item seq 从
`create.lastItemSeq+1` 连续到 10；无 `gap`；4 个 delta 的文本拼接与 complete 文本逐字
相等；delta 与 complete 共享同一 `itemId`；complete 文本等于 stub 对本次追问的回复。
订阅时 `after=5`，服务端只会补发 `seq>5` 的既有 item；本次 6..10 都是订阅之后的实时广播。

### 4.4 get（追问后）与 items GET

```text
get: state=idle, lastItemSeq=10, rootSessionId 未变
items asc (limit=2): [1,2] → [3,4] → [5,6] → [7,8] → [9,10]，hasMore=true×4，末页 false
items desc(limit=2): [10,9] → [8,7] → [6,5] → [4,3] → [2,1]，hasMore=true×4，末页 false
full asc: count=10, firstSeq=1, lastSeq=10, hasMore=false, nextCursor=null
```

每个 item 的完整内容与 SSE 一致；追问回答的 `message:complete` 在 items 中可按
`itemId`/`seq` 与 SSE 对齐。SDK 的 `CursorPage` 本身按 OpenAI 形状解析
（`body.data` / `body.has_more`），对 Swarm 响应读到的 `data.length=0`、
`has_more=false`，因此分页由显式的 `after=nextCursor` 与 `body.hasMore` 驱动；该差异
原样记录，未改写网关契约。

## 5. 分页/游标断言（实际执行）

| 断言 | 结果 |
| --- | --- |
| 每页 `items.length <= limit` | 通过（limit=2，每页 2 条） |
| `hasMore=true` 时 `nextCursor` 非空 | 通过（2/4/6/8；desc 9/7/5/3） |
| asc 游标严格 `seq > after` | 通过（无重复） |
| desc 游标严格 `seq < after` | 通过（无重复） |
| asc 覆盖 `1..lastItemSeq` 每项一次 | 通过（10 条） |
| desc 是 asc 的逆序 | 通过 |
| 每页 `logicalId`/`lastItemSeq` 稳定 | 通过（10） |
| 末页 `hasMore=false`、`nextCursor=null` | 通过 |

## 6. 两库 schema 与版本（运行后查询）

查询方式：Hrana v3 pipeline（`POST /db/<tenant>/<database>/<branch>/v3/pipeline`），
只读 SQL；凭证只在 `Authorization` 头。

会话库 `local_default/demo/main`：

```text
SELECT singleton, schema_version, contract FROM peri_store_meta;
  ['0', '15', 'peri.session.store/v9']

SELECT type, name FROM sqlite_master WHERE name LIKE 'swarm%' ORDER BY name;
  （0 行）
```

状态库 `local_default/swarm/main`：

```text
SELECT version FROM swarm_meta;
  ['1']

sqlite_master（限定对象）：
  index  swarm_items_by_id
  table  swarm_bindings
  table  swarm_inputs
  table  swarm_items
  table  swarm_meta
  table  swarm_sessions

SELECT logical_id, root_session_id, state, model, last_item_seq FROM swarm_sessions;
  6 行（脚本 6 次运行各一会话），state=idle、model=stub-model、last_item_seq=10

SELECT logical_id, COUNT(*), MIN(seq), MAX(seq) FROM swarm_items GROUP BY logical_id;
  每个会话 10 条，seq 1..10

SELECT logical_id, input_id, state FROM swarm_inputs;
  每个会话 1 条追问回执，state=accepted
```

这同时是「真实组件」证据：两条写入路径分别落在两个独立 Loom 逻辑库，会话库没有
Swarm 表，状态库只保留 Swarm 自己的 schema 版本。

## 7. 断言来源：真实组件 vs 模型 stub

| 断言 | 来源 |
| --- | --- |
| HTTP 路由、Bearer/`OpenAI-Beta` 门禁、状态码 | 真实 `peri-swarm` 进程（axum） |
| 会话行、item seq/内容、input 回执、两库 schema | 真实 Loom（`peri-loom-v1`/`peri-platform`） |
| actor 生命周期、SSE 实时广播、idle/running 状态 | 真实 Rivet engine 3.1.1 + `SwarmActor` |
| 进程内 ACP 会话、`session/new`、流式通知、title/prediction helper | 真实 Peri ACP host |
| 回答文本、delta 数量、模型调用次数 | 本地模型 stub（`stub-echo:<sha1>#<n>`） |
| SSE 增量文本与 complete 文本一致 | Peri 投影逻辑（真实）+ stub 文本（可控） |
| 会话/分页/游标语义 | 真实网关与真实存储 |

模型 stub 的行为：`POST /v1/chat/completions`（Peri 固定 `stream:true`）返回 4 段 SSE
文本和 `finish_reason=stop`；含 `<prediction_directive>` 的请求计为 helper，不推进
主轮次计数。本次运行 `mainCalls=2`、`helperCalls=2`，证明 title/prediction helper 没有
打乱主轮次。

## 8. 与官方 Agents API 形状的差异（记录，不改契约）

1. **events POST**：官方高层 body 是 `{events:[{type,input}, …]}`；Swarm 网关是「一次
   一个提交」，字段为 `inputId`/`kind`/`mode`/`text`/`attachments`/`pluginSelections`/
   `originalDraft`，且 `deny_unknown_fields`。因此 `events.create` 被 400 拒绝；同一官方
   client 的 `post` 被用于发送网关 body。没有修改网关来兼容两种形状。
2. **items 分页形状**：官方 `CursorPage` 读 `data`/`has_more`，Swarm 返回
   `items`/`hasMore`/`nextCursor`/`lastItemSeq`。SDK 的自动翻页因此不可用；显式
   `after`/`limit`/`order` 请求与响应断言均正常。
3. **会话对象字段**：官方 `AgentSession` 使用 `status`、`created_at`、`last_active_at`
   等；Swarm 返回 `state`、`createdAt`、`lastActivityAt`（另有 `logicalId`、
   `rootSessionId`）。`sessions.retrieve` 返回原始 JSON，断言按 Swarm 字段进行。
4. **create 的 `stream:true`**：官方高层 create 在流式模式下解析
   `agent.session.created` 等事件；本阶段用非流式 create（首轮同步完成），SSE 由
   `events.stream` 覆盖。

## 9. 失败与偏差记录（都已处置）

1. **首次启动 `-32010 workspace location is unavailable`**：`PERI_SWARM_WORKSPACE_ROOT`
   用了 `/tmp/peri-swarm-s9/workspace`，而 `resolve_cwd` 会 `canonicalize` 成
   `/private/tmp/...`；`RemoteWorkspaceEnvironment::contains` 做词法前缀比较，于是认为
   cwd 不在根下。改为 `/private/tmp/peri-swarm-s9/workspace`（macOS 上同一目录）后通过。
   这是部署配置的真实约束，不是本阶段代码缺陷；后续阶段应使用 canonical 根。
2. **Loom 拒绝 755 的 run-dir**：`dev.py` 要求 runtime 目录私有（0700）且不是符号链接；
   预创建的目录需 `chmod 700`。首次尝试的 755 目录被显式拒绝，未产生半启动状态。
3. **继承的 Langfuse 环境**：宿主 shell 设有 `LANGFUSE_*`，ACP host 会尝试启用 telemetry，
   构造失败时报告 `telemetry_failed`。正式运行用 `env -u LANGFUSE_*` 隔离；与 S4 测试的
   `clear_telemetry_env` 同理。
4. **8080 被用户进程占用**：OrbStack 在会话开始前就监听 `127.0.0.1:8080`（PID 78876），
   本阶段未触碰它，改用 `127.0.0.1:18080`。结束时 8080 仍由同一 OrbStack 进程监听。

## 10. teardown 与残留核对

```text
kill -INT <peri-swarm>      → 11 s 内退出；日志 "remote Peri session store closed"、
                              "swarm state store closed"；exit 0
rivet-engine                → 随网关退出自行结束（6420 无监听）
kill <model stub>           → node 进程退出（SIGTERM，exit 143；9496 无监听）
Loom                        → 第一次收尾 SIGTERM dev.py，dev.py exit 0；
                              最终收尾先 SIGTERM peri-platform 再停 dev.py，
                              dev.py 因其子进程已先停止而 exit 1；
                              authority/worker 子进程随之退出（9497 无监听）

lsof 18080/9496/9497/6420   → 0 个 LISTEN
pgrep peri-swarm|local_model_stub|rivet-engine|dev.py  → 无匹配
lsof 8080                   → 仍为 OrbStack PID 78876（运行前同一进程，未触碰）
```

网关退出时 `rivet registry` 在 10 s 宽限期后被 abort（`task did not stop within the
grace period; aborting component="rivet registry"`），两个 store 均已正常关闭；engine 随后
自行退出，未遗留进程。

## 11. 耗时（run5 / run6）

| 阶段 | run5 | run6 |
| --- | --- | --- |
| create（含首轮） | 4050 ms | 3185 ms |
| get | 57 ms | 26 ms |
| SSE 打开 | 55 ms | 27 ms |
| SSE 打开 → 追问 POST | 202 ms | 201 ms |
| 追问 POST（含该轮） | 2366 ms | 1984 ms |
| SSE 读取到 terminal | 2623 ms | 2212 ms |
| items（asc+desc+全量） | 394 ms | 385 ms |

## 12. 未验证项

- 沙箱 / `environment.type=openai_hosted` / Bun 桥（S9a2）。
- 工具调用、MCP 注入、审批与工具过滤（S9a2）。
- 断连重连、进程重启后的 `session/load`/恢复、慢客户端 `gap`（S9a3）。
- 多 Actor 并发、跨 Actor 调度、1000 Agent 吞吐（不在本地文本验收范围）。
- 真实模型供应商；本记录的所有文本均来自本地 stub。
- 官方 Agents API 的完整形状（`sessions.stream`、`AgentSession.status`、
  `items.data`/`has_more`）未实现兼容层；差异见 §8。

## 13. 复现

```bash
cd /Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev
./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm

mkdir -p /tmp/peri-swarm-s9/loom-run && chmod 700 /tmp/peri-swarm-s9/loom-run
cd /Users/konghayao/code/ai/peri-loom
PYTHONDONTWRITEBYTECODE=1 PERI_HRANA_EXPERIMENTAL=1 \
  PERI_RUST_BINARY=/tmp/peri-env-probe/loom/peri-loom-v1 \
  PERI_PLATFORM_BINARY=/tmp/peri-env-probe/loom/peri-platform \
  python3 prototypes/v1/platform/dev.py \
    --run-dir /tmp/peri-swarm-s9/loom-run --listen 127.0.0.1:9497 --no-build

cd /Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev
python3 peri-swarm/scripts/provision_state_db.py \
  --connection-file /tmp/peri-swarm-s9/loom-run/connection.json

PERI_STUB_PORT=9496 node peri-swarm/scripts/local_model_stub.mjs &

TOKEN=$(python3 -c "import json;print(json.load(open('/tmp/peri-swarm-s9/loom-run/connection.json'))['project_api_key'])")
env -u LANGFUSE_PUBLIC_KEY -u LANGFUSE_SECRET_KEY -u LANGFUSE_HOST \
    -u LANGFUSE_BASE_URL -u LANGFUSE_USER_ID -u LANGFUSE_TRACE_SAMPLING \
  PERI_SWARM_BIND=127.0.0.1:18080 \
  PERI_SWARM_API_KEY=s9a1-test-key \
  PERI_SWARM_SESSION_STORE=http://127.0.0.1:9497/db/local_default/demo/main/ \
  PERI_SWARM_STATE_STORE=http://127.0.0.1:9497/db/local_default/swarm/main/ \
  PERI_SWARM_STORE_ENGINE=turso \
  PERI_SWARM_STORE_TOKEN_ENV=PERI_SWARM_STORE_TOKEN \
  PERI_SWARM_STORE_TOKEN="$TOKEN" \
  PERI_SWARM_MACHINE_ID=019a6e4c-0000-7000-8000-0000000009a1 \
  PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-swarm-s9/workspace \
  PERI_SWARM_SETTINGS_JSON='{"config":{"active_alias":"sonnet","providers":[{"id":"local-stub","type":"openai","apiKey":"test-key-not-a-credential","baseUrl":"http://127.0.0.1:9496/v1"}],"profiles":{"sonnet":{"provider":"local-stub","model":"stub-model"}}}}' \
  PERI_SWARM_MODEL=stub-model \
  RIVETKIT_ENGINE_AUTO_DOWNLOAD=1 \
  ./target/debug/peri-swarm

npm install --prefix /tmp/peri-s9a1-sdk openai@7.62.0
NODE_PATH=/tmp/peri-s9a1-sdk/node_modules \
  PERI_SWARM_BASE_URL=http://127.0.0.1:18080/v1 \
  PERI_SWARM_API_KEY=s9a1-test-key \
  PERI_SWARM_MODEL=stub-model \
  PERI_MODEL_STUB_URL=http://127.0.0.1:9496 \
  node peri-swarm/scripts/sdk_sessions_e2e.mjs
```

> 说明：`/tmp` 在 macOS 上是指向 `/private/tmp` 的符号链接；`PERI_SWARM_WORKSPACE_ROOT`
> 必须写 canonical 形式（见 §9.1）。若本机 8080 空闲，可将 `PERI_SWARM_BIND` 与
> `PERI_SWARM_BASE_URL` 改回 8080，其余不变。
