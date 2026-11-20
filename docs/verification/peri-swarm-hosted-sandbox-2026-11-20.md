# Peri Swarm hosted 沙箱工具执行验收（S9a2）

- 状态：验收记录（一次真实本地运行）
- 日期：2026-11-20
- 工作树：`/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- 基线：`5853eae5c4ce7f1ee92a355d99620c0947d65c54`（S1..S9a1）
- 范围：真实 `openai_hosted` 预留、真实 SRT 沙箱、模型驱动的 workspace `Bash`
  工具调用、文件落盘、session events/items、teardown。模型侧为本地确定性
  stub；A/B/C 共享环境与故障恢复不在本记录内（S9a3）。
- 复现脚本（本阶段新增/扩展，已入库）：
  `peri-swarm/scripts/sdk_hosted_tools_e2e.mjs`（新）、
  `peri-swarm/scripts/local_model_stub.mjs`（扩展 tool call）。
  运行产物、连接文件和日志在 `/tmp/peri-swarm-s9a2/`（不入库）。
- 记录中的所有 token 均为一次性测试值，正文以 `<test-*>` 代替；仓库与本文档
  不包含真实凭据。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 本地 Peri 二进制可构建并被 sandbox provider 使用 | 通过 | §2：`target/debug/peri`，sha256 `0e512f4b…`，`peri 0.2.0` |
| Bun 懒预留不启动 SRT | 通过 | §5.1：`POST /v1/sandboxes {lazy:true}` → `reserved`、`runId:null`、无目录、0 个 SRT 进程组 |
| **session 创建/发现阶段 start 计数为 0（S9a2 运行）** | **未达成（已被 §10 修复）** | §6：创建返回时已是 `ready`、`runId=run_…`、1 个进程组，模型主调用为 0；原因见 §6 |
| **session 创建/发现阶段 start 计数为 0（S9a3a 修复后）** | 通过 | §10.4：创建返回 `reserved`、`runId:null`、0 进程组；6 s 静默窗口后仍 `reserved`；首个真实 `tools/call` 才启动 |
| 首个非冷可答请求后 start 计数为 1 | 通过 | §6：`resources/list` 启动首个实例；模型 `Bash` 调用复用同一 `runId`，全程仅 1 个进程组 |
| `GET /v1/sandboxes/{envId}` 状态 ready 并返回 workspacePath | 通过 | §5.2：`state=ready`、`providerVersion=0.2.0`、绝对路径 |
| `<workspacePath>/hello.txt` 存在且内容为 `hello-from-swarm` | 通过 | §5.3：17 字节，raw 为 `hello-from-swarm\n`，trim 后逐字相等 |
| session events/items 包含工具调用与结果、最终文本到达 | 通过 | §5.4：8 条 item（tool_call ×2、tool_result、4 个 delta、complete） |
| 沙箱是真实 SRT 进程而非宿主目录 | 通过 | §5.5：provider 是 SRT CLI 子进程、沙箱 HOME/TMPDIR、宿主 `bindings.json` 读取被拒 |
| teardown 只清理本测试数据，无端口残留 | 通过 | §7：DELETE 本 env、404、0 进程、根目录目录列表不变 |
| Bun 回归 | 通过 | §8：`test:swarm` 30/30、`tsc --noEmit`、`format:check` |
| 复现脚本断言 | 通过 | 75 条断言全部通过（含冷/热两条路径），见 §5 |

核心链路成立：模型产出的 `Bash` 调用经过 Peri workspace MCP → Bun 反向代理 →
SRT 中的真实 Peri provider，命令在沙箱内执行并写回
`~/.peri-swarm/sandboxes/<sandboxId>/workspace/hello.txt`。

原验收断言假设“首个 `tools/call` 才触发启动”。实测首个触发器是宿主在 session
组装期的 `resources/list`，不是模型 `tools/call`；因此“创建阶段 start=0”在
session 级不成立。本文不把该断言写成通过，也不为它引入会牺牲 workspace 资源面
的临时改动，原因与候选处置见 §6。

> 后续（S9a3a，§10）：上述触发点已修复——“创建/握手/工具发现不启动，首个真实调用
> 才启动”。§2–§9 保留 S9a2 的原始运行与未达成证据，不重写历史；修复后的运行与
> 时间线见 §10。

## 2. 组件与版本

| 组件 | 版本/标识 |
| --- | --- |
| Peri 工作树 HEAD（运行前） | `5853eae5c4ce7f1ee92a355d99620c0947d65c54` |
| Peri 二进制 | `/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev/target/debug/peri` |
| Peri 二进制 sha256 | `0e512f4b9988d46db21ef8de55c67509442d3273361e1ecfcfa8f94e224b332b` |
| Peri 二进制大小/版本 | 307362424 bytes；`peri 0.2.0` |
| 构建命令 | `./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri`（dev，已是最新） |
| Bun | 1.4.2 |
| Node | v26.9.0 |
| Peri Loom | `/Users/konghayao/code/ai/peri-loom` @ `12dd01a15985634396dcdc10a49212e38c16702b` |
| Loom 二进制 | `/tmp/peri-env-probe/loom/peri-loom-v1`、`/tmp/peri-env-probe/loom/peri-platform`（预构建） |
| Rivet engine | `~/.rivetkit/engine/v3/3.1.1/rivet-engine-aarch64-apple-darwin`（Rivet 3.1.1） |
| 官方 SDK | `openai@7.62.0`（`npm install --prefix /tmp/peri-swarm-s9/sdk`） |
| 模型 | 本地 stub（`stub-model`），不访问外网 |

`periPath` 通过 Bun 进程的 `PERI_PATH` 指向上述 worktree 二进制；未写
`~/.peri-sandbox/settings.json`，走 `loadSettings()` 的默认 settings（`PERI_PATH`
覆盖默认 `~/.peri/peri`，`periSha256` 在加载时对该文件重新计算）。`sandboxesRoot`
由 swarm manager 显式传参，固定为 `~/.peri-swarm/sandboxes`（0700）。

## 3. 启动命令（真实执行）

### 3.1 Loom（两库前置）

```bash
mkdir -p /tmp/peri-swarm-s9a2/loom && chmod 700 /tmp/peri-swarm-s9a2/loom
cd /Users/konghayao/code/ai/peri-loom
PYTHONDONTWRITEBYTECODE=1 PERI_HRANA_EXPERIMENTAL=1 \
  PERI_RUST_BINARY=/tmp/peri-env-probe/loom/peri-loom-v1 \
  PERI_PLATFORM_BINARY=/tmp/peri-env-probe/loom/peri-platform \
  python3 prototypes/v1/platform/dev.py \
    --run-dir /tmp/peri-swarm-s9a2/loom --listen 127.0.0.1:9497 --no-build
```

readiness：`{"ready": true, "service": "local-project", "address": "http://127.0.0.1:9497", …}`。

第二库：

```bash
python3 peri-swarm/scripts/provision_state_db.py \
  --connection-file /tmp/peri-swarm-s9a2/loom/connection.json --json
# state_locator=http://127.0.0.1:9497/db/local_default/swarm/main/
# session_locator=http://127.0.0.1:9497/db/local_default/demo/main/
```

### 3.2 模型 stub（带 tool call）

```bash
PERI_STUB_PORT=9496 PERI_STUB_CHUNK_DELAY_MS=5 \
PERI_STUB_TOOL_COMMAND='echo hello-from-swarm > hello.txt; if cat /Users/konghayao/.peri-swarm/sandboxes/bindings.json >/dev/null 2>&1; then echo sandbox=escaped; else echo sandbox=denied; fi' \
  node peri-swarm/scripts/local_model_stub.mjs
```

tool call 使用 OpenAI chat completions SSE 的 `delta.tool_calls`：

```text
data: {"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_stub_1","type":"function","function":{"name":"Bash","arguments":""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo hello-from-swarm > hello.txt; …\"}"}}]}}]}
data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
data: [DONE]
```

工具名直接取请求 `tools[]` 中的 `Bash`（workspace 是远端 MCP，工具面为直接名；
兼容回退到 `mcp__<server>__Bash`），参数形状以 cold-tools 快照的 `Bash.command`
为准。stub 以 `<prediction_directive>` 识别 Peri 的 prediction/title helper：本次
helper 1 次，`mainCalls=2`、`toolCalls=1`，helper 未计入主轮次。

### 3.3 Bun swarm 服务

```bash
cd npm-packages/peri-sandbox
PERI_SWARM_BUN_BIND=127.0.0.1:8081 \
PERI_SWARM_BUN_TOKEN=<test-bun-token> \
PERI_SWARM_SANDBOXES_ROOT=/Users/konghayao/.peri-swarm/sandboxes \
PERI_PATH=/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev/target/debug/peri \
  bun run src/swarm/main.ts
```

启动记录：

```json
{"event":"swarm_server_listening","url":"http://127.0.0.1:8081",
 "sandboxesRoot":"/Users/konghayao/.peri-swarm/sandboxes","auth":true,"pid":59533}
```

### 3.4 网关（Loom 双库 + Rivet + Bun bridge）

```bash
TOKEN=$(python3 -c "import json;print(json.load(open('/tmp/peri-swarm-s9a2/loom/connection.json'))['project_api_key'])")
env -u LANGFUSE_PUBLIC_KEY -u LANGFUSE_SECRET_KEY -u LANGFUSE_HOST \
    -u LANGFUSE_BASE_URL -u LANGFUSE_USER_ID -u LANGFUSE_TRACE_SAMPLING \
  PERI_SWARM_BIND=127.0.0.1:18080 \
  PERI_SWARM_API_KEY=<test-gateway-key> \
  PERI_SWARM_SESSION_STORE=http://127.0.0.1:9497/db/local_default/demo/main/ \
  PERI_SWARM_STATE_STORE=http://127.0.0.1:9497/db/local_default/swarm/main/ \
  PERI_SWARM_STORE_ENGINE=turso \
  PERI_SWARM_STORE_TOKEN_ENV=PERI_SWARM_STORE_TOKEN \
  PERI_SWARM_STORE_TOKEN="$TOKEN" \
  PERI_SWARM_MACHINE_ID=019a6e4c-0000-7000-8000-0000000009a2 \
  PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-swarm-s9a2/workspace \
  PERI_SWARM_SETTINGS_JSON='{"config":{"active_alias":"sonnet","providers":[{"id":"local-stub","type":"openai","apiKey":"test-key-not-a-credential","baseUrl":"http://127.0.0.1:9496/v1"}],"profiles":{"sonnet":{"provider":"local-stub","model":"stub-model"}}}}' \
  PERI_SWARM_MODEL=stub-model \
  PERI_SWARM_BUN_URL=http://127.0.0.1:8081 \
  PERI_SWARM_BUN_TOKEN=<test-bun-token> \
  RIVETKIT_ENGINE_AUTO_DOWNLOAD=1 \
  ./target/debug/peri-swarm
```

`PERI_SWARM_WORKSPACE_ROOT` 必须是 canonical `/private/tmp/...`（S9a1 §6.1）。
8080 仍为用户 OrbStack，网关沿用 18080。

### 3.5 驱动

```bash
NODE_PATH=/tmp/peri-swarm-s9/sdk/node_modules \
  PERI_SWARM_BASE_URL=http://127.0.0.1:18080/v1 \
  PERI_SWARM_API_KEY=<test-gateway-key> \
  PERI_SWARM_MODEL=stub-model \
  PERI_MODEL_STUB_URL=http://127.0.0.1:9496 \
  PERI_SWARM_BUN_URL=http://127.0.0.1:8081 \
  PERI_SWARM_BUN_TOKEN=<test-bun-token> \
  PERI_SWARM_SANDBOXES_ROOT=/Users/konghayao/.peri-swarm/sandboxes \
  PERI_E2E_PERI_PATH=/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev/target/debug/peri \
  node peri-swarm/scripts/sdk_hosted_tools_e2e.mjs
```

## 4. 本次运行标识

| 字段 | 值 |
| --- | --- |
| session id / logicalId | `agent-01a1f9c2-5855-7301-b89c-61b092caeab5` |
| rootSessionId | `01a1f9c2-5861-78c3-a576-cd4e74d88aac` |
| environment | `{"type":"openai_hosted","id":"env_01a1f9c2-5899-7122-9de6-0ee03e13d1b0"}` |
| envId | `env_01a1f9c2-5899-7122-9de6-0ee03e13d1b0` |
| sandboxId | `sbx_4a7f67ae765aeb1ef85dfa96bb59fcf5` |
| runId | `run_504297fa4be558cd4c5b89e5` |
| workspacePath | `/Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f67ae765aeb1ef85dfa96bb59fcf5/workspace` |
| session cwd（宿主） | `/private/tmp/peri-swarm-s9a2/workspace/agent-01a1f9c2-5855-7301-b89c-61b092caeab5` |
| toolCallId | `call_stub_1` |
| 总耗时 | 4.746 s（`create` 1494 ms；events POST→SSE completed 2857 ms） |

## 5. 关键证据

### 5.1 Bun 懒预留（真实服务，非 fake）

直接对 Bun 发起一次独立预留（不经过 Peri session）：

```json
POST /v1/sandboxes {"logicalEnvId":"s9a2-lazy-bb52b825-…","lazy":true}
→ {"sandbox":{"state":"reserved","runId":null,
   "sandboxId":"sbx_d4fb0b5bfd70639290fec61d52c62c2d",
   "workspacePath":"/Users/konghayao/.peri-swarm/sandboxes/sbx_d4fb…/workspace", …}}
```

- `workspacePath` 与 `sbx_<id>` 目录均不存在；
- 匹配该 workspace 的 SRT 进程 0 个、进程组 0 个；
- `DELETE …?deleteData=true` → `{"closed":false,"deletedData":true}`，目录未残留。

结论：预留本身不会启动 SRT（与 S8a3a 的 lazy 契约一致）。

### 5.2 创建后的 sandbox 视图

```json
{"logicalEnvId":"env_01a1f9c2-5899-7122-9de6-0ee03e13d1b0",
 "sandboxId":"sbx_4a7f67ae765aeb1ef85dfa96bb59fcf5",
 "state":"ready",
 "runId":"run_504297fa4be558cd4c5b89e5",
 "workspacePath":"/Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f…/workspace",
 "mcpUrl":"http://127.0.0.1:8081/sandboxes/env_01a1f9c2-5899-…/mcp",
 "lastError":null,
 "providerVersion":"0.2.0"}
```

注意：创建返回时已是 `ready`，不是 `reserved`；原因见 §6。

### 5.3 文件

```text
path: /Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f…/workspace/hello.txt
bytes: 17
raw: "hello-from-swarm\n"
trimmed: "hello-from-swarm"
mode: 644  uid: 501
```

宿主 session cwd 下没有 `hello.txt`，文件只出现在 sandbox workspace。

### 5.4 items 与 SSE

items（`order=asc&limit=200`，共 8 条）：

| seq | kind | 关键字段 |
| --- | --- | --- |
| 1 | `tool_call` | `toolCallId=call_stub_1`，`title=Bash`，`status=in_progress` |
| 2 | `tool_call` | 同上，`input.command` 为完整 Bash 命令 |
| 3 | `tool_result` | `toolCallId=call_stub_1`，`status=completed`，`output="sandbox=denied\n"` |
| 4..7 | `message` | 4 个 `delta`，共享同一 `itemId` |
| 8 | `message` | `complete=true`，`text=stub-echo:2290a03a27be#2` |

SSE：10 个事件，turn states `started → completed`，item seqs `1..8`，最终文本与
stub 第 2 次主调用回复逐字相等。stub 统计：`mainCalls=2`、`helperCalls=1`、
`toolCalls=1`；第 2 次主调用 `toolResultSeen=true`。

### 5.5 真实 SRT 证据

`ps -ax -ww -o pid=,ppid=,pgid=,command=`：

```text
pid 60636 pgid 60636 ppid 59533  bun …/@anthropic-ai/sandbox-runtime/dist/cli.js
  --settings /Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f…/policy.json
  -- /Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev/target/debug/peri
     mcp-start peri --capability workspace --stdio
     --workspace /Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f…/workspace
pid 60638 pgid 60636 ppid 60636  target/debug/peri mcp-start … --workspace /Users/konghayao/.peri-swarm/sandboxes/sbx_4a7f…/workspace
```

- 恰好 1 个 SRT CLI、1 个 workspace provider、1 个进程组；
- provider 的父进程是 SRT CLI（不是 Bun，也不是宿主 shell）；
- provider 的 `ps eww` 环境：`HOME=<sandboxRoot>/home`、
  `TMPDIR=/private/tmp/peri-4a7f67ae765aeb1e/claude`（沙箱私有），没有宿主
  `HOME`；
- `sandbox-exec` 不在进程表：它在应用 profile 后 `exec` 目标，进程被 Peri
  provider 取代；启动参数中的 `--settings <policy.json>` 与 `--` 后的 provider
  命令由 SRT CLI 持有；
- 运行时隔离反证：同一 Bash 调用尝试读取宿主
  `/Users/konghayao/.peri-swarm/sandboxes/bindings.json`（policy 的 `denyRead:["/"]`
  未放行它），结果为 `sandbox=denied`，而 `hello.txt` 写入成功。

`policy.json` 的 `filesystem.allowWrite` 包含该 workspace，`allowRead` 包含
provider 二进制与该 workspace；`bindings.json`、`policy.json` 等沙箱根元数据不在
allowRead 中。

### 5.6 teardown

```json
DELETE /v1/sandboxes/env_01a1f9c2-5899-7122-9de6-0ee03e13d1b0?deleteData=true
→ {"logicalEnvId":"env_…","closed":true,"deletedData":true}   (162 ms)
GET  → 404 sandbox_not_found
```

- 该 workspace 的 SRT 进程组 0 个；
- `~/.peri-swarm/sandboxes` 下目录列表与运行前一致（运行前为空）；
- 只删除了本次 env 的 sandbox；同根下其他数据未触碰；
- 9496/9497/6420/18080/8081 在本记录完成后按 §7 停止并核对无本任务残留；
- 8080 仍为用户 OrbStack（未触碰）。

## 6. 发现：session 创建阶段无法保持 cold

### 6.1 观测

`POST /v1/agents/sessions`（`environment.type=openai_hosted`，无 input）返回时：

```text
createMs = 1494
sandbox.state = ready
sandbox.runId = run_504297fa4be558cd4c5b89e5
SRT 进程组 = 1
stub.mainCalls = 0
```

模型没有运行，SRT 却已经启动。因此“创建/发现阶段 start=0”在 session 级未达
成；只能用 §5.1 的 Bun 预留 API 证明预留本身是懒的。

### 6.2 原因（wire + 源码）

临时在 Bun 的 `proxyMcp` 打印请求 method 的诊断 run（探针未提交，已移除）给出
了首帧顺序：

```text
server/discover      (cold)
tools/list           (cold)
resources/list       (cold answer 不存在 → manager.ensure → 启动 SRT)
resources/list       (已转发到热 provider)
tools/call           (已转发)
resources/read × n   (已转发)
```

即启动触发器是 `resources/list`，不是模型的 tool call，也不是
`shell_task_list`。源码依据：

- `peri-middlewares/src/mcp/initialize.rs` 的启动发现流程在 `tools/list` 之后
  调用 `list_all_resources_cached_for_startup`；
- 对 `ConfigSource::WorkspaceRemote`，`resources/list` 失败会走
  `fail_workspace_resource_discovery`，该连接不发布为 ready；
- 因此 cold 面必须真实回答 `resources/list` 才能让 workspace 连接就绪，而 cold
  面当前只承诺 tools 能力、不持有资源快照；回答空列表会让 workspace 资源/技能面
  在整个 session 内保持为空（没有启动后的重新枚举）。
- `workspace_watch.rs` 的 `shell_task_list` 是另一个 host-only 探针；本次 wire
  顺序中它发生在启动之后，但同样属于“非模型发起的 workspace 调用”。

### 6.3 影响与处置

- 本阶段没有为了让断言变绿而伪造 start 计数，也没有把 `resources/list` 改成空
  列表（那会牺牲 workspace 资源/技能面）。记录真实行为：预留是 lazy 的，但
  session 组装的资源发现会在首个模型调用前启动 SRT。
- 若后续要求 session 创建阶段严格 `reserved`，需要在 S9a3 做设计选择，候选：
  1. 对 `WorkspaceRemote` 延迟 `resources/list`，在沙箱首次启动后重新发现；
  2. cold 面回答资源清单并在启动后触发重发现/`list_changed`；
  3. 接受 eager start，修改 S8a3a 的 lazy 契约与文档。
- 本记录不替上述选择做决定；A/B/C 共享环境与恢复同属 S9a3。

## 7. teardown 与门禁

- 停止顺序：driver teardown → peri-swarm → Bun → stub → Loom。
- Rivet engine 由 gateway 拉起，但在 gateway 退出后仍短暂存活，本次按 PID 单独
  停止（`~/.rivetkit/engine/v3/3.1.1/rivet-engine-aarch64-apple-darwin`，6420）。
  该生命周期行为未在本阶段评价。
- 只删除本次创建的两个逻辑环境（session env 与 lazy probe env），均为
  `deleteData=true`；未删除 `~/.peri-swarm/sandboxes` 下其他条目（运行前后均为
  空根，`bindings.json` 为 `{"version":1,"bindings":{}}`）。
- 端口核对：8080 为 OrbStack；8081/9496/9497/6420/18080 在本任务停止后无监听。
- SRT 进程组（PID 60636/60638）与 `/private/tmp/peri-4a7f67ae765aeb1e` 私有
  tmp 均随 DELETE 清理，复核时不存在；
- 宿主 session cwd 下没有 `hello.txt`；本任务的宿主 workspace 仅保留空的
  `agent-*` 目录（`/private/tmp/peri-swarm-s9a2/workspace`，运行产物，不入库）。
- Bun：`bun run build`、`node --test dist/tests/swarm/*.test.js` 30/30、
  `bun run typecheck`、`bun run format:check` 全部通过。
- Cargo：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` 全部通过
  （203 passed；3 ignored；0 failed），包含 `openai_hosted_test` 的 7 条。
- S9a1 回归：同一扩展 stub 在 `environment.type=none` 下（请求不宣告 `Bash`）
  仍只返回文本；`sdk_sessions_e2e.mjs` 122/122 断言通过，未触发 tool call。
- `lefthook run pre-commit` 通过（feature-catalog 22、layer-imports 22 条边、
  tui-components 8）。
- 本次提交只包含本阶段脚本、验证记录和冷 discover 的 wire 修复：
  `peri-swarm/scripts/local_model_stub.mjs`、
  `peri-swarm/scripts/sdk_hosted_tools_e2e.mjs`、
  `npm-packages/peri-sandbox/src/swarm/cold-tools.ts`、
  `npm-packages/peri-sandbox/tests/swarm/cold.test.ts`、
  本文件与 S9a1 记录中的链接。

## 8. cold discover wire 修复

真实链路首先在 `server/discover` 处失败：cold 结果只有
`supportedVersions/capabilities/_meta`，rmcp 按 2026-07-28 `DiscoverResult`
反序列化失败后归为 `CustomResult`，报
`expect initialized result, but received: Some(CustomResult(…))`，在 `tools/list`
之前中止——任何工具调用都不会发生。

修复：`coldDiscoverResult` 补齐规范字段
`resultType:"complete"`、`ttlMs:0`、`cacheScope:"private"`（`supportedVersions`
仍只声明 cold 面能回答的 `2026-07-28`）。`tests/swarm/cold.test.ts` 增加字段断言，
fake provider 的 discover 响应同步为真实形状。该修复不改变工具面，只让 cold 面
符合协议 schema。

## 9. 未验证 / 局限

- 单次真实运行；未做多 session 并发、shared env（A/B/C）或恢复。
- 模型是确定性 stub，不是真实模型；验证的是 tool-call wire 与执行链，不是模型
  选择工具的质量。
- `sandbox-exec` 以 exec 方式被 provider 取代，进程表中不可见；真实隔离由
  SRT CLI 参数、沙箱 HOME/TMPDIR 和 denied-read 结果共同证明。
- `createMs` 包含预留、ACP 组装、资源发现与 SRT 启动，未单独拆分启动耗时。
- 原始 `e2e.json`、进程快照、wire 探针日志在 `/tmp/peri-swarm-s9a2/`，不入库；
  本文档保留了断言结论与关键字段。

## 10. S9a3a 修复：冷 workspace 资源探测延后到首次真实调用

- 状态：追加验证（2026-11-20，S9a2 记录的后续修复）
- 基线：`48f8e7101`（S1..S9a2）+ 本阶段改动；工作树
  `/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- Peri 二进制：`target/debug/peri`（`peri 0.2.0`），sha256
  `93ccfb0b168fd6a47a67f06dc99c3a09f4f0ae3fc1a6b73f36504ba4ce0b2cc9`
- 环境：Bun 1.4.2、Node v26.9.0、Peri Loom `12dd01a1…`、`openai@7.62.0`，
  其余与 §2/§3 相同；本次运行目录 `/private/tmp/peri-swarm-s9a3a/`。

### 10.1 问题与修复点

S9a2 的首个启动触发器是宿主在 session 组装期发出的 `resources/list`（§6.2）。
本次按“宿主启动期的可选工作区探测延后到实际需要；动态资源内容仅在真实后端读取”
处理，只改宿主侧，cold 面的 `server/discover` / `tools/list` 与 Bun 转发逻辑不变：

| 触发点 | 修复 |
| --- | --- |
| 启动发现的 `resources/list`（`mcp::initialize`） | `ConfigSource::WorkspaceRemote` 启动期不再探测；连接以 cold 工具面发布，资源快照为空 |
| 内容准入的指令/覆盖面（`read_builtin_workspace_instructions` / `read_builtin_workspace_meta`） | cold 远端 workspace 返回“不可得/空”，不探测 |
| 技能发现的 legacy `resources/templates/list`（`run_ensure_discovery`） | cold 远端 workspace 不参与技能投影，不 spawn 发现 |
| 恢复侧 `shell_task_list`（`workspace_watch`） | cold 期跳过扫描，首个真实调用后恢复 |
| git 归因 `before_agent` 的 `workspace/gitBranch`（`workspace_io`） | cold 期返回 `Unavailable`，首个真实调用后恢复 |
| 首个真实 `tools/call`（`tool_bridge`） | 置位 `workspace_remote_started`；随后做**一次**启动后 `resources/list` 重发现，失败只 WARN（`downgrade_resource_listing`），不改变连接、不失败 session |

本机 builtin `workspace` 与普通 MCP 行为不变；`fail_workspace_resource_discovery`
及其“真实失败”判定仍用于 reconnect / OAuth 等真实失败路径，本次未改写。

### 10.2 wire 顺序（本次运行，Bun `mcp_request`）

```text
（session/new，cold）
server/discover       (cold 应答)
tools/list            (cold 应答)
（创建返回，reserved；6 s 静默窗口内无任何转发请求）
（首个 prompt）
tools/call            (Bash；Bun 此时 ensure → SRT 启动)
resources/list        (启动后一次性重发现)
resources/templates/list（置位后的下一次技能发现）
```

cold 期没有出现 `resources/list`、`resources/templates/list`、
`workspace/gitBranch` 或 `shell_task_list`。

诊断说明（临时探针，已移除、未提交）：在给 `workspace/gitBranch` 加门之前，
真实链路里的首个 ensure 由 `before_agent` 的 `workspace/gitBranch` 触发
（2026-11-20T17:12:29.927Z），说明该探针同样是“会话期可选探测”；加门后
ensure 只由首个 `tools/call` 触发。

### 10.3 start 计数

| 阶段 | Bun 视图 | runId | SRT 进程组 | 模型主调用 |
| --- | --- | --- | --- | --- |
| create 返回 | `reserved` | `null` | 0 | 0 |
| create + 6 s 静默窗口 | `reserved` | `null` | 0 | 0 |
| 首个 `tools/call` 后 | `ready` | `run_88ad8cdd18ed90248a752bf2` | 1 | 2（首轮 tool_call + 最终文本） |
| DELETE 后 | 404 | — | 0 | — |

- `createMs=1072`、`eventsPostMs=3173`、`sandboxReadyMs=3175`（本次运行）；
- 恰好 1 个 SRT CLI（pgid 56833）与 1 个 Peri workspace provider（ppid 56833）；
- 沙箱根在 create 阶段不存在；`policy.json` 创建于 17:19:56.665Z，
  `hello.txt` 写于 17:19:56.807Z，说明 SRT 由首个 `tools/call` 启动。

### 10.4 启动后资源探测

- 首次真实调用后 host 日志：

```text
远端 workspace 首次真实调用已转发；开始启动后资源重发现 server=workspace
远端 workspace 启动后资源重发现完成 server=workspace resources=15
```

- 结果非空（15 条），来自真实后端（cold 代理在转发前已启动 provider）；
- 已知边界：本阶段只记录/缓存这次发现，`McpClientHandle.resources` 仍是 cold
  快照（面板与按句柄快照投影的资源面不会在会话内刷新）；刷新句柄需替换代数与
  影响发现证据，留待后续阶段裁决。

### 10.5 其他验收项（本次运行）

- `hello.txt`：17 bytes，raw `hello-from-swarm\n`，mode 644；
- items：8 条（tool_call ×2、tool_result、4 delta、complete），SSE 10 事件；
- 模型 stub：`mainCalls=2`、`toolCalls=1`、`helperCalls=1`；
- tool result 原文：`sandbox=denied: cat: <sandbox>/home/.peri-swarm/sandboxes/bindings.json:
  No such file or directory`（宿主路径在沙箱 HOME 下不可解析）；
- teardown：DELETE 200 / `deletedData:true` / GET 404 / 0 进程组；
  端口 8081/9496/9497/6420/18080 全部关闭，`~/.peri-swarm/sandboxes` 仅剩空
  `bindings.json`；
- 驱动断言：81/81 通过（含 `creation stage is reserved`、`sandbox stays reserved
  for 6000ms before the first prompt`、`startup deferred the remote workspace
  resource probe`、`the post-start resource probe finished visibly`）。

### 10.6 局限

- 单次真实运行；A/B/C 共享环境、恢复、多 session 并发仍未覆盖；
- 模型是确定性 stub；
- hosted session 的冻结 system prompt 不再包含远端工作区指令/覆盖（cold 期不可得，
  显式降级）；本机 builtin workspace 不受影响；
- 启动后资源发现结果未回写句柄快照（见 §10.4）；
- 原始 `e2e.json`、Bun/gateway 日志在 `/private/tmp/peri-swarm-s9a3a/logs/`，
  不入库。

## 11. S9a3b1：A/B/C 共享 `openai_hosted` 环境验收

- 状态：真实链路验收（2026-11-20，S9a3a 之后的共享环境阶段）
- 基线：`8c01f97fa`（S1..S9a3a）+ 本阶段脚本改动；工作树
  `/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- Peri 二进制：`target/debug/peri`（`peri 0.2.0`），sha256
  `93ccfb0b168fd6a47a67f06dc99c3a09f4f0ae3fc1a6b73f36504ba4ce0b2cc9`
  （未重建；本阶段未改 Rust 源码）
- 环境：Bun 1.4.2、Node v26.9.0、Peri Loom `12dd01a1…`、`openai@7.62.0`；
  运行目录 `/private/tmp/peri-swarm-s9a3b1/`，网关端口沿用 18080
  （8080 仍是用户 OrbStack）
- 驱动：`peri-swarm/scripts/sdk_hosted_share_e2e.mjs`（新增）；
  模型 stub：`peri-swarm/scripts/local_model_stub.mjs`（扩展）

### 11.1 驱动与模型 stub

驱动通过官方 `openai@7.62.0` Agents API 创建三个会话，每个会话都用真实 Bun
workspace MCP + 真实 SRT 沙箱执行 `Bash`：

| 场景 | 创建方式 | 期望 |
| --- | --- | --- |
| A | `environment:{type:"openai_hosted"}` | 懒预留；首个工具调用启动 SRT；写 `a.txt` |
| B | 同上（新环境） | 另一个 sandbox；`a.txt` 不存在；写 `b.txt` |
| C | `environment:{type:"openai_hosted", environment_id:<A 的 env id>}` | 复用 A 的 sandbox/workspace/runId；读到 `a.txt` |

模型 stub 新增 `PERI_STUB_COMMANDS`（JSON：指令名 → shell 命令）和
`[[tool:<name>]]` 输入标记：每轮首个主调用（该轮尚无 tool 结果）返回
`Bash` tool call，命令由输入选择；收到工具结果后返回最终文本。判定按
**携带指令的 user 消息**划定轮次边界，而不是“最后一条 user 消息”——Peri 每次请求
都会在末尾追加 `<system-reminder>` capability 消息，按最后一条 user 消息判定会把
工具结果永远排除在外。该判定无会话级状态，因此并发会话可以交错轮次；旧
`PERI_STUB_TOOL_COMMAND`（单次 tool turn）仅在显式设置时保留。

输入样式的例子（`write-a` 命令）：

```text
S9a3b1 scenario=A [[tool:write-a]] Run the workspace Bash command for write-a, …
```

本次运行的启动命令与 §3 相同（Loom 双库 9497、Bun 8081、网关 18080，run-dir
`/private/tmp/peri-swarm-s9a3b1`），只有两处差异：

```bash
# stub：指令表 + 与驱动共享的同一份 JSON
PERI_STUB_PORT=9496 PERI_STUB_CHUNK_DELAY_MS=5 \
PERI_STUB_COMMANDS="$(cat /private/tmp/peri-swarm-s9a3b1/stub-commands.json)" \
  node peri-swarm/scripts/local_model_stub.mjs

# 驱动
NODE_PATH=/tmp/peri-swarm-s9/sdk/node_modules \
PERI_SWARM_BASE_URL=http://127.0.0.1:18080/v1 \
PERI_SWARM_API_KEY=<test-gateway-key> \
PERI_MODEL_STUB_URL=http://127.0.0.1:9496 \
PERI_SWARM_BUN_URL=http://127.0.0.1:8081 \
PERI_SWARM_BUN_TOKEN=<test-bun-token> \
PERI_SWARM_SANDBOXES_ROOT=/Users/konghayao/.peri-swarm/sandboxes \
PERI_STUB_COMMANDS="$(cat /private/tmp/peri-swarm-s9a3b1/stub-commands.json)" \
PERI_E2E_OUT=/private/tmp/peri-swarm-s9a3b1/logs/e2e.json \
  node peri-swarm/scripts/sdk_hosted_share_e2e.mjs
```

### 11.2 A/B/C 身份

| 字段 | A | B | C |
| --- | --- | --- | --- |
| session id | `agent-01a1fa1d-e234-7450-9af3-85362291c81e` | `agent-01a1fa1d-f3f3-7870-872e-a3d51afe4827` | `agent-01a1fa1e-0fd4-71e1-a275-d3710608e340` |
| rootSessionId | `01a1fa1d-e240-7511-be85-8df63358e522` | `01a1fa1d-f401-7b71-a407-bf27ab05cf84` | `01a1fa1e-0fe4-78f2-9a66-e749dc70cad2` |
| `environment.id` | `env_01a1fa1d-e294-7241-bea9-c03b959136fc` | `env_01a1fa1d-f43e-72a3-910f-02f32f725f09` | 与 A 相同 |
| sandboxId | `sbx_65211ce9f47de65c853291b08206d54f` | `sbx_cc2ee4c708f9dfb21d75117f62ec7274` | 与 A 相同 |
| workspacePath | `~/.peri-swarm/sandboxes/sbx_6521…/workspace` | `~/.peri-swarm/sandboxes/sbx_cc2e…/workspace` | 与 A 相同 |
| runId | `run_2dac1fa230a2499161c15ff2` | `run_9df295dacea152a6d3571416` | 与 A 相同 |
| create 返回 | `reserved`、`runId:null`（1148 ms） | `reserved`、`runId:null`（1197 ms） | `ready`、A 的 `runId`（1098 ms） |
| session cwd（宿主） | `/private/tmp/peri-swarm-s9a3b1/workspace/agent-01a1fa1d-e234…` | 同前缀 `agent-01a1fa1d-f3f3…` | 同前缀 `agent-01a1fa1e-0fd4…` |

C 的创建走 `environment.environment_id` 复用路径：`environment.id` 回显 A 的
逻辑环境 id，Bun 预留返回同一个 ready 实例，`swarm_bindings` 记录同一
`sandboxId` 与 `lastRunId`（`bindings.json` 中该 env 的
`sandboxId=sbx_6521…`、`lastRunId=run_2dac…`）。

### 11.3 文件隔离与共享读取（工具结果为证）

| 断言 | 工具输出（Bash result 原文） |
| --- | --- |
| A 写 `a.txt` | `a-file=a-content`；`pwd=<A 的 sandbox workspace>` |
| B 的沙箱没有 `a.txt` | `a-absent` |
| B 写 `b.txt` | `b-file=b-content` |
| A 看不到 B 的 `b.txt`（B 写完后） | `b-absent`；`listing=a.txt,` |
| A 宿主侧的 workspace 目录 | 仅 `a.txt` |
| C 读 A 的文件 | `c-read=a-content` |
| B 宿主侧的 `b.txt` | 存在，内容 `b-content`；B 的 workspace 无 `a.txt` |

A/C 的宿主 workspace 是同一个目录；B 的 `sbx_cc2e…` 目录独立。驱动同时检查了
宿主侧文件（不只看工具输出）：A 的 `workspace` 只含 `a.txt`，B 的含 `b.txt`。

### 11.4 启动计数：A 启动一次，C 复用同一进程

| 阶段 | Bun 视图 | runId | SRT 进程组 | 进程（pid / ppid / pgid / lstart） |
| --- | --- | --- | --- | --- |
| A 创建 | `reserved` | `null` | 0 | — |
| A 首个工具调用后 | `ready` | `run_2dac…` | 1（pgid 83707） | 83707（SRT CLI，ppid=Bun 77342）、83709（Peri provider，ppid 83707），均为 `02:00:27` |
| B 首个工具调用后 | `ready` | `run_9df2…` | 1（pgid 84051） | 84051/84053，`02:00:32` |
| C 首个调用前 | `ready` | `run_2dac…` | 1（pgid 83707） | 同一对 pid |
| C 首个调用后 | `ready` | `run_2dac…` | 1（pgid 83707） | pid 与 lstart 不变 |
| 并发/取消阶段后 | `ready` | `run_2dac…` | 1（pgid 83707） | 不变 |

C 的创建没有新建 SRT：pgid、pid、`lstart`、`runId`、`bindings.json.lastRunId`
在 C 的首个 `tools/call` 前后完全一致。B 是独立的第二个进程组。

### 11.5 并发（phase 1）：A 与 C 同时输入

`POST /events` 在整轮结束后才返回（`submit_input` 内 `run_turn().await`），
因此并发必须两个请求同时在途；驱动用 `startTurn` 先发起两个 POST，再等待结果。

| 时刻（UTC） | 事件 |
| --- | --- |
| 18:00:40.979 | A、C 的 SSE 打开；两条输入在同一次 tick 内发出 |
| 18:00:41.195 | 中止 C 的 SSE 订阅（C 已收到 1 个事件） |
| 18:00:42.577 / 42.718 | stub 记录 slow-c / slow-a 的 tool turn |
| 18:00:44.006 | A 的 final turn（C 仍在执行工具） |
| 18:00:44.557 | C 的 final turn |
| 18:00:45.476 | A 的 SSE `turn:completed`（`stopReason=end_turn`） |
| 18:00:46.643 | C 的 item log 完成（订阅已中止） |

- 交错证据：C 的 tool turn 早于 A 的 final turn，A 的 tool turn 早于 C 的 final
  turn，两个会话的主调用真实重叠；两条输入的开始时间相差 0 ms。
- 工具结果：A `a-done=a-content|a-concurrent`；C `c-start=a-content`、
  `c-end=a-content|a-concurrent`（C 在同一轮内的后一次读取看到了 A 并发追加的
  内容，共享 workspace 的写入可见）。
- 订阅中止不影响执行：C 的 SSE 以 `closed` 结束（无异常），item log 仍有完整
  tool_call → tool_result → message；重新以 `after=8` 打开 SSE 可重放
  seq 9..17（`turn` 状态事件不持久化，与 §5.4 一致）。
- 消息隔离：A 的 item log 不含 `c-start=/c-end=/c-read=`，C 的 item log 不含
  `a-done=/a-file=/a-tail=`；各自完成消息（message:complete）与 stub 的 final
  turn 文本一致。
- workspace 末态：`a.txt = "a-content\na-concurrent"`（A 的追加只出现一次）。

### 11.6 取消隔离（phase 2）：取消 C 不影响 A

| 时刻（UTC） | 事件 |
| --- | --- |
| 18:00:47.951 | A、C 同时发起（`cancel-a` / `cancel-c`，后者 `sleep 20`） |
| 18:00:50.155 | C 的 tool_call 落入 item log（seq 19；stub 于 49.856 发出） |
| 18:00:50.2 左右 | `POST /events {kind:"control", mode:"turn", text:"cancel"}` 发往 C |
| 18:00:51.237 | cancel 返回 `accepted=true, disposition=inserted, lastItemSeq=20` |
| 18:00:51.987 | A、C 的 SSE 终态；随后两个 `/events` 返回 |

- C：`turn:completed`，`stopReason=cancelled`；tool_result 为
  `failed` / `interrupted by user\n[Execution status: cancelled]`；取消后没有新的
  最终消息。
- A：`stopReason=end_turn`，工具结果 `a-cancel-done=a-content|a-concurrent|a-cancel-phase`，
  final 文本来自本轮自己的 stub 回复；item log 不含 C 的输出。
- A 的工具 turn 在 C 进入被取消工具调用时仍在运行（stub 时间戳），取消没有中断
  另一个会话，也没有改变共享 SRT 进程组（pgid 83707）或 runId。
- 取消后共享 workspace 仍可用：A 的追加落盘，`a.txt =
  "a-content\na-concurrent\na-cancel-phase"`。

### 11.7 stub 计数与 teardown

- stub：`mainCalls=15`、`toolCalls=8`、`helperCalls=8`、`otherCalls=0`；
  8 个 directive 各有一个 tool turn，`cancel-c` 因在工具调用中被取消而没有 final
  turn；其余 7 个 directive 各有 tool + final（`mainCalls = 8 + 7`）。
- teardown（只删本测试的两个环境）：

```text
DELETE /v1/sandboxes/env_01a1fa1d-e294-…?deleteData=true
→ {"logicalEnvId":"env_01a1fa1d-e294-…","closed":true,"deletedData":true}   (A/C 共享环境)
GET → 404；sandbox 目录不存在；SRT 进程组 0
DELETE /v1/sandboxes/env_01a1fa1d-f43e-…?deleteData=true
→ 同上；GET → 404；0 进程组
```

- `~/.peri-swarm/sandboxes` 回到测试前基线：无 sandbox 目录，`bindings.json`
  为 `{}`；
- 端口：8081/9496/9497/6420/18080 全部关闭，仅 8080 仍为用户 OrbStack；
  `ps` 无 `sandbox-runtime` / `mcp-start` / `peri-swarm` / stub 残留；
- 驱动断言：154/154 通过（含身份、隔离、启动计数、并发、取消、teardown）。

### 11.8 观测与局限

- **观察（非本阶段修复）**：`message:complete` 可能先于最后几个
  `agent_message_chunk` 落盘。item log 仍完整：同一 `messageId` 的 complete +
  后续 delta 拼接后等于 stub 的 final 文本。本阶段 6 次完整运行中 3 次出现
  （`cLateDeltas` 分别为 1、1、2），都发生在被中止 SSE 订阅的 C 轮；A 轮的
  `aLateDeltas` 始终为 0；本次运行 `cLateDeltas=0`。机制上 `reader.rs` 的
  `settle()` 只有 15 ms 静默窗口（`INGEST_SETTLE_WINDOW`），通知分发出现 >15 ms
  抖动时会复现；与中止是否存在因果关系未证实。驱动按“complete + 同 messageId
  的后续 delta”重建文本并在证据里保留 `lateDeltas` 计数；未改网关代码。
- 模型是确定性 stub；工具命令由输入 directive 选定，不覆盖真实模型的计划能力。
- 客户端 SSE 中止（`AbortController`）与 `kind=control` cancel 是两种不同的
  “取消”路径；本阶段都只验证了“另一会话不受影响”，不验证取消后的会话复用。
- 重连、`after` 游标跨断线续读的完整语义、同一 `inputId` 的幂等重放留 S9a3b2。
- 本阶段未覆盖网关/actor 重启后的环境恢复（`environment_id` 重新绑定）。
- 原始 `e2e.json`、stub/Bun/gateway 日志在 `/private/tmp/peri-swarm-s9a3b1/logs/`，
  不入库。
