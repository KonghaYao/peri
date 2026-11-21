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
| 启动发现的 `resources/list`（`mcp::initialize`） | 显式 `lazy_workspace` 的 `ConfigSource::WorkspaceRemote` 启动期不再探测；连接以 cold 工具面发布，资源快照为空 |
| 内容准入的指令/覆盖面（`read_builtin_workspace_instructions` / `read_builtin_workspace_meta`） | lazy cold 远端 workspace 返回“不可得/空”，不探测 |
| 技能发现的 legacy `resources/templates/list`（`run_ensure_discovery`） | lazy cold 远端 workspace 不参与技能投影，不 spawn 发现 |
| 恢复侧 `shell_task_list`（`workspace_watch`） | lazy cold 期跳过扫描，首个真实调用后恢复 |
| git 归因 `before_agent` 的 `workspace/gitBranch`（`workspace_io`） | lazy cold 期返回 `Unavailable`，首个真实调用后恢复 |
| 首个真实 `tools/call`（`tool_bridge`） | lazy 宿主置位 `workspace_remote_started`；随后做**一次**启动后 `resources/list` 重发现，失败只 WARN（`downgrade_resource_listing`），不改变连接、不失败 session |

本机 builtin `workspace` 与普通 MCP 行为不变；`fail_workspace_resource_discovery`
及其“真实失败”判定仍用于 reconnect / OAuth 等真实失败路径，本次未改写。

**范围收窄（本次复核）**：最初的判定是“来源为 `WorkspaceRemote` 即延后”，会连带
覆盖普通 ACP 客户端的远端 workspace。现在只有宿主显式 `lazy_workspace`（Swarm
`AcpChild`）才跳过启动期探测；非 lazy 的 `WorkspaceRemote` 恢复既有“资源发现失败
拒绝初连”（`initialize_test::remote_workspace_resource_discovery_failure_blocks_initial_connection`），
lazy 路径的延迟与一次性重发现由 `initialize_test::lazy_remote_workspace_resources_are_deferred_until_first_real_call`、
`workspace_io_test::cold_remote_workspace_defers_git_branch_until_first_real_call` 与
`workspace_watch_test::remote_workspace_watch_defers_shell_task_list_until_first_real_call` 覆盖。
本次只做代码级回归，没有重跑 §10.2 的真实 Bun 链路。

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

## 12. S9a3b2：SSE 断流重连与输入幂等验收

- 状态：真实链路验收（2026-11-20，S9a3b1 之后的断流/幂等阶段）
- 基线：`16ce894df`（S1..S9a3b1）+ 本阶段脚本与记录；工作树
  `/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- Peri 二进制：`target/debug/peri`（`peri 0.2.0`），sha256
  `93ccfb0b168fd6a47a67f06dc99c3a09f4f0ae3fc1a6b73f36504ba4ce0b2cc9`
  （未重建；本阶段未改 Rust 源码）
- 环境：Peri Loom `12dd01a159`（9497，双库）、Bun 1.4.2（8081）、Node v26.9.0、
  网关 + Rivet engine（18080 / 6420）、模型 stub（9496）；
  运行目录 `/private/tmp/peri-swarm-s9a3b2/`
- 驱动：`peri-swarm/scripts/sdk_reconnect_idempotency_e2e.mjs`（新增，纯 HTTP/SSE；
  直接读取 SSE `id:`，不依赖 SDK 的事件对象）

### 12.1 场景与判定

| 场景 | 输入 | 判定 |
| --- | --- | --- |
| R：断流重连 | `openai_hosted`；`[[tool:slow-reconnect]]` → Bash 循环 `tick-1..6`，每步 `sleep 0.4`，末尾 `slow-done` | 读取部分 SSE 后主动断开；无订阅时 turn 继续并完成；`GET /items` 补齐；按 `Last-Event-ID`/`afterSeq` 续读无重复、无缺口 |
| I：输入幂等 | `environment:none`；同一 UUID `Idempotency-Key` + 相同文本；无 directive | 并发两次 POST 只产生一轮执行；同 key 不同内容 409 且不入队；完成后同 key 同内容重放返回同一受理 |

驱动共 79 条断言；run2/run3/run4 三次连续运行均为 79/79，通过结果一致。本节引用
run4（`/private/tmp/peri-swarm-s9a3b2/logs/e2e-run4.json`）。

### 12.2 断流时间线（run4）

| 时刻（UTC） | 事件 |
| --- | --- |
| 18:10:46.475 | 打开 SSE `GET /events?after=0`（HTTP 200） |
| 18:10:46.476 | `POST /events`（`Idempotency-Key: a5a95392-…`，`kind=user`、`mode=turn`） |
| 18:10:47.955 | stub 首轮主调用：`Bash` tool call，命令为慢循环 |
| 18:10:48.133 | SSE 已收到 item `seq=1,2`（tool_call 开始 + 带 command 的更新）；客户端 abort，`Last-Event-ID=2` |
| 18:10:48.201 | 无订阅时 `GET /items`：`[1,2]`，会话 `state=running` |
| 18:10:51.022 | items 出现 `seq=3` tool_result：`tick-1..tick-6` + `slow-done` |
| 18:10:51.097 | stub 第二轮主调用（final 文本），发生在断开之后 |
| 18:10:51.815 | items 采样到 `seq=8`：message:complete；此前 4 条 delta（4..7） |
| 18:10:52.210 | 无订阅读取确认会话 `idle`；用 `Last-Event-ID: 2` 打开新 SSE |
| 18:10:52.738 | 重连流已回放 3..8；stub final 文本与 item 折叠一致 |
| 18:10:54.687 | 幂等会话开始并发双 POST |
| 18:10:58.015 | 两个 POST 均返回 202（inserted / existing） |
| 18:10:58.107 | 同 key 不同内容 → 409 `input_conflict` |
| 18:10:58.220 | 同 key 同内容重放 → 202 `existing` |

### 12.3 断流期间的 `GET /items`

断开时（18:10:48.201）item log 只有工具调用的两行，turn 仍在运行；之后完全依靠
`GET /items` 观察到完整结果：

| seq | itemId | kind | 内容 |
| --- | --- | --- | --- |
| 1 | `itim_01a1fa27-6414-…` | tool_call | `call_stub_1`，status `in_progress` |
| 2 | `itim_01a1fa27-6468-…` | tool_call | 同一 `toolCallId`，带 `{"command": "for i in 1 2 3 4 5 6; …"}` |
| 3 | `itim_01a1fa27-6f4a-…` | tool_result | completed；`tick-1\ntick-2\ntick-3\ntick-4\ntick-5\ntick-6\nslow-done\n` |
| 4 | `itim_01a1fa27-705a-…` | message delta | `stub-e` |
| 5 | 同一 itemId | message delta | `cho:b6` |
| 6 | 同一 itemId | message delta | `e87772` |
| 7 | 同一 itemId | message delta | `fecc#2` |
| 8 | 同一 itemId | message complete | `stub-echo:b6e87772fecc#2` |

- 断开到完成之间没有 SSE 订阅；`tool_result`、4 条 delta 与 complete 全部由 items
  读出，说明输出没有依赖订阅者存活。
- Bun 日志中该工具的 `tools/call` `durationMs=2467`，即慢命令真实执行于 SRT。

### 12.4 重连游标：`Last-Event-ID` 与 `afterSeq`

- SSE1 在 abort 前收到 item id `[1,2]`，`lastEventId=2`；
- SSE2 请求头 `Last-Event-ID: 2` → 收到 item id `[3,4,5,6,7,8]`：
  - 首条为 `2+1`；连续、无重复、无缺口（`len = 8-2`）；
  - 每条回放 payload 与持久化 item 逐字段相等；
  - 0 条 `turn` 帧（turn 状态不持久化，重放只含 item）；
- SSE3 `?afterSeq=7` → 只收到 `[8]`，查询参数游标与头部游标结果一致；
- `[1,2] ∪ [3..8] = [1..8]`，每个 seq 恰好一次；
- `GET /items?afterSeq=2` 的 seq 列表与 SSE2 完全一致。

### 12.5 delta + complete 折叠与稳定 item ID

- 本轮所有 message item 共享一个 `itemId`（`itim_01a1fa27-705a…`）与一个
  `messageId`；重复读取 items 时 itemId 不变；
- 4 条 delta 的 `delta=true`，complete 行 `complete=true`；delta 文本按 seq 拼接
  等于 complete 文本，也等于 stub 的 final 回复
  `stub-echo:b6e87772fecc#2`；
- `lateDeltas=0`（run2/3/4 均为 0）；驱动仍保留“complete 后的同
  `messageId` delta 追加”折叠路径。

### 12.6 items 分页与跨重连游标

从断流游标 `afterSeq=2` 起以 `limit=2` 翻页：

| 请求 | seqs | nextCursor | hasMore | lastItemSeq |
| --- | --- | --- | --- | --- |
| `afterSeq=2` | `[3,4]` | 4 | true | 8 |
| `afterSeq=4` | `[5,6]` | 6 | true | 8 |
| `afterSeq=6` | `[7,8]` | null | false | 8 |

- 每页 `nextCursor` 等于该页最后一个 seq；末页 `hasMore=false`、
  `nextCursor=null`；三页并集 `[3..8]` 无重复/无缺口；
- 每页 `lastItemSeq=8`，与 SSE 的 `Last-Event-ID` 和 items 最终 seq 对齐。

### 12.7 模型调用数（断流 turn）

- stub `mainCalls=2`：`#1` tool_call（18:10:47.955）、`#2` final
  （18:10:51.097）；`toolCalls=1`；prediction/title helper 不计入 mainCalls；
- final 调用发生在 SSE 断开（18:10:48.133）之后约 3 s，证明断开没有中止 turn；
- 只有一个 tool turn 与一个 final turn，没有因重连触发重复执行。

### 12.8 同一 `Idempotency-Key` 并发两次

会话 `agent-01a1fa27-76c3-…`（`environment:none`），key
`f353f898-5384-4596-b5f0-ee05c2eafdfe`，请求体不含 `inputId`，仅由请求头提供：

| 请求 | 状态 | `accepted` | `disposition` | `lastItemSeq` | 耗时 |
| --- | --- | --- | --- | --- | --- |
| A | 202 | true | `inserted` | 5 | 3328 ms |
| B（并发） | 202 | true | `existing` | 0 | 391 ms |

- 两个响应都回显同一 `inputId`，都 `accepted=true`；恰好一次 `inserted`；
- B 的 `lastItemSeq=0` 是响应时刻（A 的 turn 尚未写入 item）的日志长度，
  不代表第二轮执行；A 的 5 是 turn 完成后的长度；
- 该对请求只产生 1 个 completion item（seq 5）和 1 次模型 final 调用
  （stub `mainCalls` 2→3）；
- 状态库 `swarm_inputs` 只有一行：`(input_id, accepted, SHA-256 fingerprint)`；
  `swarm_items` 为 5 行，与 items API 一致。

### 12.9 同 key 不同内容与完成后重放

| 操作 | 响应 | 追加执行 | 证据 |
| --- | --- | --- | --- |
| 同 key + 文本追加 ` different` | `409`，`error.code=input_conflict`，`details.inputId` 为该 key | 无 | items 仍为 `[1..5]`；stub `mainCalls` 3→3；会话 `idle` |
| 完成后同 key + 原文本 | `202`，`accepted=true`，`disposition=existing`，`lastItemSeq=5` | 无 | items 仍为 `[1..5]`；stub `mainCalls` 3→3 |

### 12.10 teardown 与端口

```text
DELETE /v1/sandboxes/env_01a1fa27-5a0f-…?deleteData=true
→ {"logicalEnvId":"env_01a1fa27-5a0f-…","closed":true,"deletedData":true}
GET → 404；sandbox 目录不存在；SRT 进程组 0
```

- 只删除本测试创建的 `openai_hosted` 环境；`~/.peri-swarm/sandboxes` 根目录
  与 `bindings.json` 回到测试前基线（无目录、`{}`）；
- 停止网关、Rivet engine、Bun、模型 stub、Loom（含 authority/platform/worker）；
- 端口核对：8081/9496/9497/6420/18080 无本任务监听；8080 仍为用户 OrbStack；
  `ps` 无 `peri-swarm` / `sandbox-runtime` / `mcp-start` / stub / Loom 残留。

### 12.11 门禁

```text
cargo test --locked -p peri-swarm
  lib 154；actor_runtime 3；http_contract 19；items_pagination 6；
  mcp_injection 4；openai_hosted 7；rivet_actor 1；streaming 9；
  loom_store 3 ignored；0 failed（203 passed）

npm-packages/peri-sandbox
  bun run build                  ok
  node --test dist/tests/swarm/*.test.js   30 pass / 0 fail
  bun run typecheck              ok
  bun run format:check           58 files, no fixes

lefthook run pre-commit          见提交前运行记录
git status                       仅本阶段脚本与本节记录
```

未提交 token：网关/Bun 使用本地随机测试串，Loom 项目 key 只存在于运行目录的
connection 文件与进程环境；驱动仅将其放入 Hrana 的 Authorization 头，不打印、
不写入仓库。

### 12.12 观测与局限

- 模型是确定性 stub；本阶段验证传输/续读/幂等语义，不评估真实模型的计划能力。
- 断流后重连发生在本轮已完成之后，验证的是持久化 backlog 的回放；同一连接
  “先回放再实时续读”的混合窗口由 S9a2/S9a3b1 的实时 SSE 覆盖，本阶段未重复。
- 未覆盖：网关/actor 重启后的会话与 `environment_id` 恢复；客户端断开后沙箱
  回收/重启时机；被中断的慢命令清理。按计划留 S9a4。
- 并发重复请求在原始 turn 未完成时可读到 `lastItemSeq=0`（受理时刻的日志长度）；
  驱动断言两个响应共享 `inputId` 且只有一轮执行，不要求该字段相等。
- `tool_call` 会先产生开始行、再产生带 command 的更新行，因此断开前已收到
  seq 1、2 两条；这不影响 `Last-Event-ID` 语义。
- 原始 `e2e-run2/3/4.json`、stub/Bun/gateway 日志在
  `/private/tmp/peri-swarm-s9a3b2/logs/`，不入库。

## 13. S9a4a：Bun 空闲回收与服务不可用错误可见

- 基线：`cd8c91d374b33ea9cb4b85210e31c2dde6ea9880`（S1..S9a3b2）
- 驱动：`peri-swarm/scripts/sdk_idle_unavailable_e2e.mjs`（新，模式
  `idle` / `prepare` / `bun-down` / `cleanup-env` / `loom-down`）
- 运行目录：`/private/tmp/peri-swarm-s9a4a/`；原始记录
  `logs/idle.json`、`logs/bun-down.json`、`logs/cleanup.json`、
  `logs/loom-down.json` 与 stub/Bun/gateway 日志同目录，不入库。
- 本阶段 Rust 源码未改；Bun 服务新增空闲回收与代理在途计数。

### 13.1 实现

| 文件 | 改动 |
| --- | --- |
| `npm-packages/peri-sandbox/src/swarm/manager.ts` | `PERI_SWARM_IDLE_MS` 解析（默认 30 分钟，`0` 关闭）、`idleSweepIntervalMs`（窗口 1/4，250 ms…30 s）；每个 logicalEnvId 的 `beginRequest`/`endRequest` 在途计数与 `lastUsedAt`；`sweepIdle()` 只在 `state=ready`、在途=0、空闲 ≥ 阈值时经既有 `close(deleteData:false)` 回收，保留 workspace 与 binding；`reclaiming` 与 `ensure`/`reserve`/`close` 互斥；回收成功/失败均写 JSON 日志 |
| `npm-packages/peri-sandbox/src/swarm/server.ts` | 代理入口 `beginRequest`，响应体完整交付后 `endRequest`（SSE/流式响应在流结束或取消时才计为结束）；转发失败也释放计数 |
| `npm-packages/peri-sandbox/src/swarm/main.ts` | 读取 `PERI_SWARM_IDLE_MS`，启动日志带 `idleMs`/`idleSweepIntervalMs`；定时 sweep 串行化并 `unref`；关闭时停止定时器 |
| `tests/swarm/manager.test.ts`、`tests/swarm/server.test.ts` | 回收条件、在途阻断、`0` 关闭、阈值边界、状态观察、重新 ensure 新 runId、流式响应在途 |

回收后的对外状态是 `stopped`（保留旧 `runId`、`workspacePath`，`mcpUrl=null`）；
同一 ID 的新 `POST /v1/sandboxes` 走显式 ensure，分配新 `runId` 并复用同一
`<root>/<sandboxId>/workspace`。被回收的 binding（含 `lastRunId`）不删除，只有
`DELETE ?deleteData=true` 才移除目录与 binding。

### 13.2 运行环境

沿用 S9a2/S9a3a 的真实 harness：Loom 9497（demo / swarm 双库）、Bun 8081
（`PERI_PATH=<worktree>/target/debug/peri`，sha256
`93ccfb0b168fd6a47a67f06dc99c3a09f4f0ae3fc1a6b73f36504ba4ce0b2cc9`）、
网关 18080 + Rivet 6420、模型 stub 9496。本阶段显式设置：

```text
PERI_SWARM_IDLE_MS=2000
HOME=/private/tmp/peri-swarm-s9a4a/home        # 网关
PERI_SWARM_WORKSPACE_ROOT=/private/tmp/peri-swarm-s9a4a/workspace
PERI_STUB_COMMANDS={"slow":"sleep 4; echo slow > slow.txt",
                    "quick":"echo prepared > prepared.txt"}
```

Bun 启动记录：

```json
{"event":"swarm_server_listening","url":"http://127.0.0.1:8081",
 "sandboxesRoot":"/Users/konghayao/.peri-swarm/sandboxes","auth":true,
 "idleMs":2000,"idleSweepIntervalMs":500,"pid":58010}
```

### 13.3 慢命令期间不回收（真实链路）

会话 `agent-01a1fa38-036e-77e1-9996-9e06f1e740f4`，
`environment={type:"openai_hosted",id:"env_01a1fa38-03b2-77d2-bc40-abc3ef0b1292"}`；
stub 发出 `Bash`：`sleep 4; echo slow > slow.txt`。时间线（UTC）：

| 时刻 | 事件 |
| --- | --- |
| 18:28:58.508 | 创建后 Bun 视图 `reserved`，`runId=null`，`sandboxId=sbx_aa9815dca9eb011a8c859532b339d811` |
| 18:29:00.305 | 首个真实 `tools/call` 后 `starting`（+1798 ms） |
| 18:29:00.407 | `ready`，`runId=run_a581893c284e1af17ec40ad5`（+1900 ms） |
| 18:29:02.999 | 在途探针：距 ready 2592 ms（阈值 2000 ms），`state=ready`、同一 runId、1 个进程组（pgid 71573，SRT CLI + Peri provider）、`slow.txt` 尚不存在 |
| 18:29:04.679 | 最后一个代理请求结束（Bun `lastUsedAt`） |
| 18:29:05.420 | turn 完成（SSE `turn: completed`） |
| 18:29:06.994 | `GET` 观察到 `stopped`，旧 `runId` 保留、`mcpUrl=null`；0 进程 |
| 18:29:07.330 | 同 ID 重新 ensure：`ready`，新 `runId=run_be4df77bcda20dc3859345d5`，pgid 71991 |

Bun 日志证明“在途不回收”：

```json
{"event":"swarm_idle_sweep","idleMs":2000,"reclaimed":[],
 "inFlight":["env_01a1fa38-…"]}                       // ×4 次（命令执行中）
{"event":"swarm_idle_sweep","idleMs":2000,
 "reclaimed":["env_01a1fa38-…"],"inFlight":[]}
{"event":"swarm_sandbox_reclaimed","logicalEnvId":"env_01a1fa38-…",
 "closed":true,"idleMs":2000,"lastUsedAt":"2026-11-20T18:29:04.679Z"}
```

模型侧 `mainCalls=2`、`toolCalls=1`；`events POST` 在本 harness 中于 turn 结束后
返回 `202`（`lastItemSeq=8`），驱动因此并发启动探针与 POST，而不是等 POST 返回
再采样。

### 13.4 回收后的状态、文件与 binding

- `GET /v1/sandboxes/{envId}`：`state=stopped`、`runId=run_a581893c…`、
  `workspacePath=/Users/konghayao/.peri-swarm/sandboxes/sbx_aa9815…/workspace`、
  `mcpUrl=null`；
- SRT 进程组：0（探针 1 → 回收后 0）；
- `<workspacePath>/slow.txt`：`slow\n`（5 字节，`trim` 后为 `slow`），
  sandbox 目录存在；
- `bindings.json`：该 env 的 `sandboxId` 不变、`lastRunId=run_a581893c…`、
  `workspacePath` 不变；其它 env 不受影响。

### 13.5 重新 ensure 与通过 Bun 读回文件

`POST /v1/sandboxes {"logicalEnvId":"env_01a1fa38-…"}` → `200`、`ready`、
新 `runId=run_be4df77bcda20dc3859345d5`、同一 `sandboxId`/`workspacePath`、
1 个进程组（pgid 71991）。随后经真实 Bun 代理 `tools/call`（`Mcp-Method:
tools/call`、`Mcp-Name: Bash`、2026-07-28 `_meta` 信封）执行 `cat slow.txt`：

```json
{"result":{"content":[{"type":"text","text":"slow\n"}],"isError":false,
  "_meta":{"peri.dev/tool-execution-v1":{"status":"completed","exit_code":0},
           "io.modelcontextprotocol/serverInfo":{"name":"peri","version":"0.2.0"}},
  "resultType":"complete"}}
```

即回收只停止运行态，文件与身份仍在；重新 ensure 后新 provider 读到的仍是旧
workspace 的内容。

### 13.6 Bun 不可用

`prepare` 创建会话 `agent-01a1fa38-4ea8-7852-939e-452f0115dc14`
（`env_01a1fa38-4eef-71f2-9777-f9b3d434d944`），重新 ensure 到 `ready`
后停止 Bun（8081 关闭，SRT provider 随 Bun 退出）。

**新建 `openai_hosted` 会话**（Bun 已停）：

```http
POST /v1/agents/sessions   {"environment":{"type":"openai_hosted"}, …}
502 {"error":{"code":"upstream_unavailable","message":"swarm upstream is unavailable",
              "details":{"operation":"sandbox_reserve","reason":"unreachable"}}}
```

没有创建会话、没有降级本机 agent；网关日志只记录
`bun sandbox bridge call failed`。

**已有会话的后续输入**：

| 观测 | 结果 |
| --- | --- |
| `POST /events` | `202 accepted=true`（actor 仍可用，输入被受理） |
| SSE / items | turn 正常结束（`completed`）；`tool_result` item `status=failed` |
| 错误文本（去 URL 尾部） | `MCP 服务器 "workspace" 工具 "Bash" 调用失败: Transport send error: … error sending request for url (http://127.0.0.1:8081/sandboxes…` |

错误在持久化 items 中可见（本次运行第二条失败 item 为 seq 19）。失败原因是
workspace MCP 传输不可达，不是本机替代执行；turn 仍能因 stub 模型给出最终文本
而 `completed`，但工具结果明确为失败。

**本机状态核查**：

- 临时 HOME `/private/tmp/peri-swarm-s9a4a/home` 内没有任何
  `threads.db`/`.db`/`.sqlite*`（只出现 Rivet 自己的 `.rivetkit` 目录）；
- 失败的创建在 `PERI_SWARM_WORKSPACE_ROOT` 下最多留下一个空 cwd 目录
  （如 `agent-01a1fa38-fa05-…`，0 个文件）；没有本地会话库或本地执行。

### 13.7 Loom 不可用

停止 9497（dev.py 与 worker 全部退出，端口关闭），网关与模型 stub 仍在：

| 请求 | 结果 |
| --- | --- |
| `POST /agents/sessions` | `502 {"code":"upstream_unavailable","details":{"operation":"get_session"}}` |
| `GET /agents/sessions/{id}/items` | 同上 `502 upstream_unavailable` |
| `GET /agents/sessions/{id}` | 同上 `502 upstream_unavailable` |

- 临时 HOME 仍无 `threads.db`/`.db`/`.sqlite*`，即没有回退到本机线程库，也没有
  创建第二套存储；
- 失败的创建同样只留下空的宿主 cwd 目录，无本地会话状态。
- 网关日志记录 `swarm store operation failed`/`upstream_unavailable`，不包含
  store token。

### 13.8 teardown 与端口

```text
DELETE /v1/sandboxes/env_01a1fa38-4eef-…?deleteData=true
→ {"logicalEnvId":"env_01a1fa38-4eef-…","closed":false,"deletedData":true}
GET → 404；sandbox 目录不存在；binding 已删除；SRT 进程 0
bindings.json → {"version":1,"bindings":{}}
~/.peri-swarm/sandboxes → 仅 bindings.json（无 sandbox 目录）
```

停止顺序：gateway → Bun → stub → Rivet engine（gateway 退出后仍短暂存活，按 PID
单独停止）。端口核对：8081/9496/9497/18080/6420 均无监听；8080 仍为用户
OrbStack。`ps` 无 `peri-swarm` / `sandbox-runtime` / `mcp-start` /
`local_model_stub` / `platform/dev.py` / `rivet-engine` 残留。

### 13.9 门禁

```text
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
  lib 154；actor_runtime 3；http_contract 19；items_pagination 6；
  mcp_injection 4；openai_hosted 7；rivet_actor 1；streaming 9；
  loom_store 3 ignored；0 failed（203 passed）

npm-packages/peri-sandbox
  bun run build                  ok
  node --test dist/tests/swarm/*.test.js   37 pass / 0 fail
  bun run typecheck              ok
  bun run format:check           58 files, no fixes

lefthook run pre-commit          见提交前运行记录
git status                       仅本阶段脚本/实现/本节记录
```

驱动断言：`idle` 40/40、`bun-down` 7/7、`cleanup-env` 5/5、`loom-down` 8/8。

### 13.10 未验证与局限

- **后台任务引用未实现**：Bun `forward.ts` 尚未识别 `resultType:"task"` 与
  `tasks/get` 终态，回收条件不含“任务台账非空”，本记录不声称已覆盖该路径。
- 回收按“最后一个代理请求结束”计时；若 turn 在最后一个工具调用后仍有很长的
  模型输出（无 MCP 在途）或之后再次调用工具，sandbox 可能已回收，代理对该
  env 的后续调用返回 `404 sandbox_not_found`，需要重新 ensure。本阶段未把
  会话状态接入回收判定。
- 只终止 Rust 节点后的恢复（`session/resume`/`state=recovering`）未在本阶段
  验证，按要求留 S9a4b。
- 网关在 Bun/Loom 全停时仍受理 `POST /events` 并完成 turn（错误体现在
  `tool_result`），这是 actor 与 sandbox 分离的既有语义；本阶段只要求“错误
  可见”，未要求 `POST /events` 同步返回 502。
- 模型仍是确定性 stub；慢命令时长为 4 s，阈值为 2 s，未覆盖分钟级空闲与
  大量并发 env 的扫描成本。
- 驱动脚本未提交任何 token；Bun/网关 token 为运行目录内的一次性随机串，
  Loom 项目 key 只从 connection 文件读入进程环境。



## 14. S9a4b：只杀 Rust 宿主后的会话恢复

- 基线：`176040715`（S1..S9a4a）
- 驱动：`peri-swarm/scripts/sdk_restart_boundary_e2e.mjs`（新，模式
  `prepare` / `pre-kill` / `after-restart` / `probe`）
- 运行目录：`/private/tmp/peri-swarm-s9a4b/`；原始 JSON
  `logs/r6-*.json`、`round1/after-restart.json` 与服务日志同目录，不入库。
- 本阶段 Rust 改动：`AcpChild::attach`（`initialize` → `session/load`）与
  `AcpSwarmService::ensure_live`；未改 Bun、Loom 与模型 stub。

### 14.1 场景

```text
Loom 9497（demo / swarm 双库）
Bun 8081（真实 SRT，PERI_PATH=target/debug/peri，PERI_SWARM_IDLE_MS=0）
模型 stub 9496（PERI_STUB_COMMANDS={"once":"printf 'once\n' >> ran.log; printf 'once\n' > once.txt"}）
网关 18080（target/debug/peri-swarm，Rivet engine 6420）
```

1. 创建 `openai_hosted` 会话，跑一轮工具调用（`ran.log` 恰好追加一行）；
2. 记录会话视图、全部 items 页、Bun `runId`、SRT 进程组、`ran.log` 的
   bytes/sha256/mtime、stub 计数与 Bun 日志指纹；
3. `kill -9 <peri-swarm pid>`，保留 Loom、Bun、SRT、stub 与 Rivet engine；
4. 用同一环境重启 `peri-swarm`；
5. 只做历史读取（`GET /agents/sessions/{id}` 与分页 `GET /items`），再
   `POST /events` 发一条**纯文本**输入（无工具指令）；
6. 对照重启前的 items、runId、进程组、文件与 Bun 日志。

### 14.2 实现（最小 reattach）

| 位置 | 行为 |
| --- | --- |
| `actor/runtime.rs` `AcpChild::attach` | 同一套 host 装配 + `initialize`，然后 `session/load`（不带 `cursor`） |
| `session_load_params` | `{sessionId: 持久 root id, cwd, mcpServers[]}`；workspace 条目与 caller 工具条目和 `session/new` 完全一致，不写 `_meta`（身份只来自 `sessionId`） |
| `actor/service.rs` `ensure_live` | 无行 → `Ok(false)`；`state=running` → `502 session_resume_required`；idle → 重建 host 并登记，`attach_lock` 串行化并发交互 |
| `reattach_workspace` | 先读 `swarm_get_binding`（缺失即失败，不新建环境），再 Bun 懒 `reserve`；sandboxId/workspacePath 变化 → `sandbox_binding_changed` |
| `actor/swarm.rs` `Submit` | 首个 turn 前调用 `ensure_live`；`GET` 仍是只读，不 reattach |
| `actor/service.rs` `create_session` | 已有行且无 host 时同样 reattach，再执行可选 `initial_input` |

禁止项保持不变：不调用 `session/resume`、不 prompt、不重放 MQ 输入、不把
`session/load` 的回放帧写入 `swarm_items`（projection 的 `periReplay` 过滤）、
不为旧行新建 root id，也不在 binding 丢失时静默换 workspace。

### 14.3 重启前边界（实现前，pre-change binary）

会话 `agent-01a1fa41-4de7-7ce2-b843-4dea20a3101c`，
root `01a1fa41-4df2-78c1-aa6b-706e1f050b7b`，env
`env_01a1fa41-4e40-7ba2-ab6f-47683aac7162`，sandbox
`sbx_1befdb5a8386818117b3d2445aec9ab2`，`runId=run_26f04ac4bbe5e4247f98c291`，
SRT pgid `3817`。

```text
18:39:16.3Z  kill -9 3059              # 只杀 peri-swarm；18080 关闭
18:39:21.7Z  新 peri-swarm (4558) listening
18:39:32.9Z  on_start: restored a read-only view（last_item_seq=8, state=Idle）
18:39:33.x   POST /events → 502 upstream_unavailable, details.operation=session_not_live
```

| 检查 | 结果 |
| --- | --- |
| `GET /agents/sessions/{id}` | `200`，同 id/rootSessionId/environment，`state=idle`、`lastItemSeq=8` |
| `GET /items`（limit=3） | 页 1-3 → cursor 3；4-6 → 6；7-8 → `null`；与重启前逐项相同 |
| sandbox `runId` / SRT 进程组 | `run_26f04…` / pgid 3817，均未变化 |
| `ran.log` | sha256 `ad06e727…`、mtime 未变、1 行 |
| stub 计数 | `mainCalls=2`、`toolCalls=1`，未变 |
| 新输入 | **502**（S5 记录为 503；当前 `upstream_unavailable` 映射 `ErrorKind::BadGateway` → 502），`session_not_live` |

该轮证明“只读历史可读、输入仍拒绝”的既有边界，也暴露文档中的 503 与实际
502 的差异；驱动已按实际响应接受 502/503 并保留 `operation=session_not_live`。

### 14.4 恢复后（实现，最终轮）

会话 `agent-01a1fa4c-08a7-7690-9574-3cb8caf40898`，
root `01a1fa4c-08b2-7063-ba19-d26862280f9a`，env
`env_01a1fa4c-08f5-7320-8ae5-de3e33ed0f47`，sandbox
`sbx_8d30a9ee496915ef5082b53c463441cc`，`runId=run_92989567da11223d41ed5bc2`，
SRT pgid `40762`（pid 40762 SRT CLI、40764 provider）。

```text
18:50:37.1Z  第一台 peri-swarm (40041) listening；prepare 完成
18:50:56.5Z  Bun 日志最后一次变化（pre-kill 指纹 1107 bytes，
             sha256 bf9814a6…）；runId/pgid/文件均与 prepare 相同
18:50:59.3Z  kill -9 40041            # 只杀 peri-swarm
18:51:00.3Z  Bun 32081 / stub 32189 / Loom 36500 / Rivet 3135 仍在监听
18:51:0x    新 peri-swarm (41524) listening
18:51:14.6Z  on_start: restored a read-only view（last_item_seq=8, state=Idle）
18:51:16.0Z  session/load 完成：reattached the persisted session
18:51:17.9Z  新 turn `completed`（requestId 01a1fa4c-7114-7680-8cb7-d7286065a773）
```

重启后、任何会话 GET 之前 Bun 日志为 `1107 bytes`；完成
`GET session` + 5 页 `GET items` 之后仍为 `1107 bytes`。`POST /events` 触发
reattach 后为 `1334 bytes`（+1 行），即历史读取没有触达 Bun。

| 检查 | 结果 |
| --- | --- |
| `GET session` | `200`，同 id/rootSessionId/environment，`state=idle`、`lastItemSeq=8` |
| `GET items`（3 页） | `[1,2,3]→3`、`[4,5,6]→6`、`[7,8]→null`，seq/itemId/kind 与重启前完全一致 |
| sandbox | `runId=run_92989567…`、`sandboxId=sbx_8d30a9ee…`、pgid 40762 与 pid 40762/40764 全部不变 |
| `ran.log` / `once.txt` | sha256 `ad06e727…`、mtime 与 1 行均不变 |
| stub | `mainCalls=2`、`toolCalls=1`（工具调用仍恰好 1 次，无命令重放） |
| 新输入 | `202 accepted=true`，SSE `turn: completed`（`end_turn`） |
| 新 items | seq 9-13 追加（`message`，同一 `itim_01a1fa4c-755a…` 的 delta+complete）；旧 1-8 保持前缀，未重复 |
| 新 turn 的模型调用 | `mainCalls 2 → 3`（恰好 +1），`toolCalls 1 → 1`；请求含持久历史，但只回复文本 |
| 新 turn 后进程 | 同一 SRT 进程组 40762，未新增 |
| Bun 日志 | `1107 → 1107`（历史读取）→ `1334`（reattach 后） |

item 序列（重启前后一致；括号内为 `itemId` 前缀）：

```text
1-2 tool_call (itim_01a1fa4c-126f / 12c5)
3   tool_result
4-8 message    (itim_01a1fa4c-14f4，delta + complete)
9-13 message   (itim_01a1fa4c-755a，新 turn 的 delta + complete)
```

### 14.5 未完成项与限制

- **未完成 turn 未恢复**：行 `state=running` 时 `ensure_live` 返回
  `502 session_resume_required`，没有调用 `session/resume`。按计划 §6.4 的
  “仅有未完成 turn 证据才 resume”，该分支仍缺失，本记录不声称覆盖崩溃中的
  turn 续跑。
- reattach 是**交互触发**：`GET` 不改变状态，只有 `POST /events`（或重复
  create）会装载 host。计划中的 `state=recovering` 未实现。
- reattach 会执行 `session/load` 的装配期探测（Bun 日志中可见
  `resources/list` 等），但不执行任何历史工具调用；`ran.log`/`once.txt`
  的 sha256 与 mtime 不变是“命令未重放”的直接证据。
- 首轮（实现前）的 `target/debug/peri-swarm` 在重建时被覆盖，未记录其
  sha256；边界结论来自该轮 JSON 与 gateway 日志。实现后二进制 sha256
  `3c518b41d89a3edf235716eed73d653361f39ca4f429b74bd76edff2a0f2b5c0`。
- 运行期间后台 Loom 包装器到达 10 分钟超时被终止（其子进程一度存活）；
  最终轮改用新的 `loom2` 运行目录重新启动 Loom。除此之外环境与驱动未变；
  第 1-4 轮使用 `loom` 目录，最终证据轮使用 `loom2`。
- 模型仍是确定性 stub；未覆盖并发 reattach、Bun 同时重启、Loom 不可用时的
  reattach 失败文案与“崩溃时正在执行命令”的中断语义。

### 14.6 teardown

```text
DELETE /v1/sandboxes/<env>?deleteData=true   # 6 个本阶段 env 全部 deletedData=true
bindings.json → {"version":1,"bindings":{}}
~/.peri-swarm/sandboxes → 仅 bindings.json
```

停止 gateway → Bun → stub → Loom（dev.py + platform + workers）→ Rivet engine。
端口核对：8080 仍为 OrbStack；8081/9496/9497/6420/18080 无监听；
无 `peri-swarm`/`sandbox-runtime`/`mcp-start peri`/stub/Loom/Rivet 残留。

### 14.7 门禁

```text
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
  lib 156；actor_runtime 3；http_contract 19；items_pagination 6；
  mcp_injection 4；openai_hosted 7；rivet_actor 1；streaming 9；
  loom_store 3 ignored；0 failed（205 passed）

npm-packages/peri-sandbox
  bun run build                  ok
  node --test dist/tests/swarm/*.test.js   37 pass / 0 fail
  bun run typecheck              ok

./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -W clippy::all
  ok（0 warning）
cargo fmt --all                 已执行
lefthook run pre-commit         见提交前运行记录
```

驱动断言：round1 `after-restart` 19 项只读检查通过（输入按实际 502 记录）；
round6 `after-restart` 30/30 通过，`prepare` / `pre-kill` 全通过。
