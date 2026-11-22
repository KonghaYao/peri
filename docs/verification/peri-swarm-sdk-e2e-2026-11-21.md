# Peri Swarm 官方 SDK e2e 驱动修复与真实栈运行

- 状态：真实栈复验通过（`openai@7.60.0` + 本地 Loom + 本地 Rivet engine + 本地模型
  stub）；脚本 exit 0，**293/293 断言通过**，原始帧 gap 计数 0/0/0。
- 日期：2026-11-21
- 运行时父提交：`5cf7cf31fcb941a58613283cef75626d9d738857`（复验修复前的 HEAD；
  本记录与修复脚本在同一后续提交中）
- 脚本 SHA-256（运行前 = 运行后 = 提交 blob）：
  `995ba00c829e28d20afeaf5f1fd540c953b3758ea87d2da99899f577c4e86299`
- 运行证据：
  - 完整 JSON：`/private/tmp/peri-swarm-sdk-e2e/e2e-run.json`
  - 原始 SSE 帧：`/private/tmp/peri-swarm-sdk-e2e/e2e-raw-frames.json`
  - stdout 副本：`/private/tmp/peri-swarm-sdk-e2e/e2e-stdout.json`
- 范围：修复 `peri-swarm/scripts/sdk_sessions_e2e.mjs` 的复验阻塞项，并用真实
  Loom 会话库/状态库、真实 Rivet engine 和真实官方 SDK 进程重新消费网关。
- 不在本记录内：真实模型供应商、吞吐/并发验收、跨机部署、真实 `turn.failed` 运行时
  路径、取消是否真正中断在途 provider（见 §5、§7）。

## 0. 证据链（运行脚本与提交脚本同一性）

脚本在运行开始和结束各计算一次自身 SHA-256，并读取 `git rev-parse HEAD`：

```json
"script": {
  "sha256AtStart": "995ba00c829e28d20afeaf5f1fd540c953b3758ea87d2da99899f577c4e86299",
  "sha256AtEnd":   "995ba00c829e28d20afeaf5f1fd540c953b3758ea87d2da99899f577c4e86299",
  "sha256Stable": true
},
"git": {
  "parentCommitAtStart": "5cf7cf31fcb941a58613283cef75626d9d738857",
  "parentCommitAtEnd":   "5cf7cf31fcb941a58613283cef75626d9d738857",
  "parentCommitStable": true
},
"exitCode": 0,
"assertionCount": 293,
"rawFrameCounts": { "create": 21, "followUp": 21, "cancel": 21 },
"rawGapCounts":   { "create": 0,  "followUp": 0,  "cancel": 0 }
```

运行前后没有修改脚本；`script blob is unchanged during the run` 与
`git parent commit is available and unchanged during the run` 是最后的断言。提交后
核对命令：

```bash
git show HEAD:peri-swarm/scripts/sdk_sessions_e2e.mjs | shasum -a 256
```

该结果应与上面的 `995ba00c...` 一致（交付报告记录实际输出）。

## 1. 复验修复项

| # | 复验结论 | 修复 |
| --- | --- | --- |
| 1 | **Blocker：gap 断言恒真**。原检查在按 `turn_id` 过滤后的数组上运行，真实 gap 帧 `{fromSeq,toSeq}` 会被过滤掉 | 新增 `isGapFrame` / `assertNoRawGap`，在 create、follow-up、cancel 的**原始未过滤帧**上检查；任何 `fromSeq`/`toSeq`/`type:"gap"` 都失败。原始帧类型计数、gap 帧、前 10 帧样本写入运行 JSON；全部原始帧写入独立文件 |
| 2 | 取消断言允许 `completed || cancelled`，实际是 `completed/end_turn`，断言名暗示已实现 cancelled | 断言改名为 `cancel request reached a terminal state`；只要求终态是 `completed` 或 `cancelled`，并在 `differences` 记录真实 `completed/end_turn`。没有改服务端，也没有把取消语义写成已验证 |
| 3 | `turnTerminalReached` 用“任意属于该 turn 的事件”判断终止，可能被回放顺序误导 | 改为读取 terminal 帧自身派生的 turn id（`agent.session.idle`/`agent.session.failed` 的 `event_id`）并与目标 turn 比较；`assertTurnIdentity` 要求每个官方 turn 帧都能解析出同一个 turn id；新增 `assertTerminalTurnIds`，对 `turn.completed`/`turn.failed`/`idle`/`session.failed`/`error` 的终态帧显式断言 turn id |
| 4 | 存在“双方都 undefined 也能通过”的断言 | 对 `rootSessionId`、`has_more`/`hasMore`/SDK `has_more`、item id/seq、turn 持久化状态等改为先要求明确的 string/number/boolean，再比较 |
| 5 | 缺少脚本/提交同一性证据 | 运行前计算脚本 SHA-256 与父提交，运行后重新计算并断言一致；JSON 记录 `exitCode`、`assertionCount`、原始帧计数与 gap 计数；提交后核对 `git show HEAD:...` |
| 6 | 运行 JSON 有 11 条 `differences`，旧记录只列 9 行 | 本节 §5 与 JSON 的 11 条逐项对齐（同名、同观察、同处置） |

## 2. 环境

| 项 | 值 |
| --- | --- |
| 主机 | macOS 26.6.2（arm64），Docker 29.8.2 |
| worktree | `/Users/mino/code/peri/.worktrees/swarm-sdk-e2e` |
| 构建 | `./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm`（`CARGO_HOME=/tmp/cargo-home-rsproxy`） |
| Loom | `ghcr.io/konghayao/peri-loom@sha256:aa72d8742e6162c11de539eb7e99d8a0934f5d14cd8f2d80b017c2fdd7ae53f1`，`--network host`，`--backend local`，`--listen 127.0.0.1:9497`，volume `peri-loom-data`，run-dir `/data/peri/run4` |
| Rivet engine | 3.1.1，`127.0.0.1:6420` |
| 模型 | `peri-swarm/scripts/local_model_stub.mjs`，`127.0.0.1:9496`，`PERI_STUB_CHUNK_DELAY_MS=2000`（仅用于扩大取消窗口） |
| 网关 | `target/debug/peri-swarm`，`127.0.0.1:8080`，`environment.type=none` |
| SDK | `openai@7.60.0`（`/Users/mino/code/peri/.claude/dogfood/node_modules`） |

Loom 使用同一 volume 此前未使用过的新 run-dir `/data/peri/run4`，避免硬删除容器后旧 run-dir 的
`owned stop proof set incomplete or unexpected`。Rivet 二进制仍来自
`@rivetkit/engine-cli-darwin-arm64@3.1.1`（registry.npmjs.org）并通过
`RIVET_ENGINE_BINARY_PATH` 指定；网关复用已启动 engine 时设置
`PERI_SWARM_RIVET_ENGINE_SPAWN=never`。服务端代码未修改。

## 3. 实际运行

```bash
NODE_PATH=/Users/mino/code/peri/.claude/dogfood/node_modules \
  PERI_SWARM_BASE_URL=http://127.0.0.1:8080/v1 \
  PERI_SWARM_API_KEY='<gateway-bearer-key>' \
  PERI_SWARM_MODEL=stub-model \
  PERI_MODEL_STUB_URL=http://127.0.0.1:9496 \
  PERI_E2E_OUTPUT_PATH=/private/tmp/peri-swarm-sdk-e2e/e2e-run.json \
  PERI_E2E_RAW_PATH=/private/tmp/peri-swarm-sdk-e2e/e2e-raw-frames.json \
  node peri-swarm/scripts/sdk_sessions_e2e.mjs
```

结果：`EXIT=0`，`ok=true`，`assertionCount=293`，失败断言 0。

## 4. 真实输出摘要

最终 session：`agent-01a1ffa6-123c-7c43-8fa5-bc9c477e13cc`，
`rootSessionId 01a1ffa6-12a5-7bb0-8992-a7df3175321d`。

### 4.1 create 流

首帧 `agent.session.created`；21 帧类型计数：

```text
agent.session.created 1, gateway.session 1, gateway.item 6,
agent.session.in_progress 1, agent.session.turn.created 1,
agent.session.turn.in_progress 1, agent.session.turn.item.added 2,
gateway.turn.started 1, agent.session.turn.content_part.added 1,
agent.session.turn.output_text.done 1, agent.session.turn.content_part.done 1,
agent.session.turn.item.done 1, gateway.turn.completed 1,
agent.session.turn.completed 1, agent.session.idle 1
```

- turn 开始顺序：`session.in_progress` → `turn.created` → `turn.in_progress`。
- assistant `turn.item.done`：`phase=final_answer`、`output_index=0`、
  `output_text="stub-echo:17496866c226#1"`，item id
  `itim_01a1ffa6-1865-71d1-9149-ad9fa2f86e5d`。
- SDK `finalResult()`（同一 create 流）：`output_text="stub-echo:17496866c226#1"`，
  `turn_id=turn_01a1ffa6-1528-7ad1-9356-3207e23ab7af`，`status=completed`，
  `messages=1`。
- 原始帧 gap 计数：`0`；原始样本首帧的 `agent.session.created` 的 `turnId` 为
  `null`（session 帧不是 turn）。

### 4.2 follow-up turn

- 只提交一次 follow-up：低层 `client.post` native body；202 receipt 回显
  `inputId`，`disposition=inserted`，`lastItemSeq` 从 6 增至 7。
- 原始 21 帧（含 actor 回放上一 turn 的 `turn.completed`/`session.idle`），按本次
  `turn_id` 过滤后 19 帧；完整官方顺序：
  `session.in_progress` → `turn.created` → `turn.in_progress` → user `item.added`
  → assistant item/part/text/done → `turn.completed` → `session.idle`。
- gateway item seq 7..12 连续；SSE complete 文本 `stub-echo:59acd9f12e99#2`；
  assistant `turn.item.done` 的 item id/seq 与 gateway complete 行一致。
- `GET` 在 `session.idle` 后 28 ms 仍看到 `state=idle, status=in_progress`，
  100 ms 时稳定为 `idle/idle`；脚本有界轮询并记录。

### 4.3 items / turns / 时间字段

- 全量 asc：12 条原始行、4 条 `data`（4 个 itemId），`lastItemSeq=12`；
  `items.length=12 === lastItemSeq`，seq 1..12 无缺口。
- `data` 每项是该 item 的最大 seq 行；`data.length ===` 页内不同 itemId 数；
  `has_more`/`hasMore`/SDK `has_more` 三者一致，且都是显式 boolean。
- asc/desc `limit=1` 各 4 页；助手 item 有 5 条原始行时该页 `data` 仍为 1 条，
  原始行数可超过 `limit`（记录，不作为失败）。
- SDK 自动分页（`for await` + `data[last].id` 作为 `after`）返回 4 条 item 级行，
  与全量 `data` 顺序一致。
- `created_at`/`last_active_at` 为 Unix 秒；旧 `createdAt`/`lastActivityAt` 不存在。
- `turns.list` 返回 3 条 turn，均 `status=completed`；turn id 与 create、
  follow-up、cancel 三条流一致；`has_more`/`hasMore` 一致。

### 4.4 取消探针（记录差异）

`control=cancel` 在 `turn.created` 到达后 **0 ms** 发出：

```text
turnStartedAtMs=428, controlSentAtMs=428, controlReturnedAtMs=7723, terminalAtMs=7723
officialStatus=completed, gatewayStopReason=end_turn, sessionStatus=idle
```

断言名为 `cancel request reached a terminal state`：请求被受理、SDK 读取器未抛错、
流无 gap、session 最终 idle。真实 Peri host 没有中断在途 provider 流，turn 以
`completed` 结束，不是 `cancelled`；取消是否真正生效在本栈上**未验证**。

## 5. 记录差异（与运行 JSON 的 11 条逐项对齐）

| # | 差异（JSON `name`） | 本次观察 | 处置 |
| --- | --- | --- | --- |
| 1 | 模型 stub 的失败注入能力 | `directives=[]`；stub 无独立失败开关；未知 `[[tool:...]]` 指令返回 400 | 记录；脚本用 `control=cancel` 覆盖取消探针，不声称能注入 turn 失败 |
| 2 | agent.session.idle 与 GET status 的时序 | idle 后 28 ms 仍为 `in_progress`，100 ms 稳定 `idle` | 有界轮询；记录 settle 窗口 |
| 3 | 会话 usage | `session.usage=null` | 记录；不断言有值 |
| 4 | after=tail 回放上一 turn 的 terminal | 原始流重放上一 turn 的 `turn.completed`/`session.idle` | 按本次 `turn_id` 过滤；原始流单独记录 |
| 5 | turn 用量 | `item.done`/`turn.completed`/`turn.usage` 均 `null` | 记录；网关不做 token 记账 |
| 6 | item.added.output_index | `null` | 记录；仅 `item.done` 携带 `output_index` |
| 7 | 官方文本 delta 帧 | 无 `output_text.delta` | 记录；文本由自定义 `item` 帧与 `item.done` 的完整 `output_text` 表达 |
| 8 | 真实 provider 流上的 control=cancel 终态 | `completed`/`end_turn`，未中断在途 provider | 记录；只断言终态可收敛，不声称实现 `cancelled` |
| 9 | turn 的子 agent 与 usage | `agent_id` 恒 `root`、`subagent_id` 恒 `null`、`usage` 恒 `null` | 记录；本地运行时只有 root agent |
| 10 | 官方 session 的 environment 投影 | 官方帧无 `environment` 字段 | 记录；environment 只在自定义 `session` 帧/GET 中出现 |
| 11 | 模型 400 与 turn 终态 | 未知 `[[tool:...]]` 指令使 stub 返回 400，ACP prompt 仍返回正常 stop reason，turn 以 `completed` 结束且无 assistant 输出 | 记录；未触发 `turn.failed` 路径 |

## 6. 原始 gap 检查与失败退出自检

原始帧 gap 检查使用未过滤的 `create.events`、`followUpRun.events` 和
`cancelled.events`，任何 `fromSeq`、`toSeq` 或 `type:"gap"` 都直接失败。本次结果：

```text
rawGapCounts: create=0, followUp=0, cancel=0
rawFrameCounts: create=21, followUp=21, cancel=21
```

运行 JSON 还包含每个流的 `rawEventTypes` 与前 10 帧 `rawFrameSamples`；全部原始帧
保存在 `e2e-raw-frames.json`。因此“流无 gap”的结论由原始检查支撑，不依赖按
turn 过滤后的数组。

失败退出用同一 `assertNoRawGap` 自检：

```bash
NODE_PATH=/Users/mino/code/peri/.claude/dogfood/node_modules \
  PERI_E2E_CONTRACT_SELF_TEST=clean node peri-swarm/scripts/sdk_sessions_e2e.mjs
# exit 0
NODE_PATH=/Users/mino/code/peri/.claude/dogfood/node_modules \
  PERI_E2E_CONTRACT_SELF_TEST=gap node peri-swarm/scripts/sdk_sessions_e2e.mjs
# exit 1，error: assertion failed: self-test: raw frames carried no gap event
```

## 7. 证据局限

1. 模型是本地确定性 stub；不证明真实供应商的 token 用量、错误分类或取消中断。
2. **`turn.failed` → `error` → `session.failed` 运行时路径未触发。** provider 400 被
   真实 host 折叠成 completed 空 turn；control cancel 也没有中断在途 provider 流。
   该路径目前只有 `actor::reader`/`streaming_test` 的契约测试证据。
3. **取消语义未验证。** control cancel 被受理且终态可收敛，但真实 turn 仍是
   `completed/end_turn`；没有证明 `cancelled` 中断。
4. 只有一次最终成功运行（另有复验前的开发运行）；不证明稳定性、并发或吞吐。
5. Loom 与 Rivet engine 都在本机；不证明跨机部署、TLS、鉴权轮换或重启恢复。
6. `cargo test -p peri-swarm` 使用内存 store/fake service；真实 Loom 持久化只由本
   e2e 的 items/turns 读取间接覆盖。
7. 官方 SDK 只验证 `7.60.0`；其他版本客户端行为未验证。
8. 旧 `createdAt`/`lastActivityAt` 只验证响应不再出现；历史数据迁移与旧客户端兼容
   未验证。
9. 未运行全量 lefthook：干净基线 worktree 的 `mcp-packages/peri-browser-gateway`
   存在既有 `cargo fmt` 失败，按任务要求不修改那些文件。

## 8. 门禁

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `node --check peri-swarm/scripts/sdk_sessions_e2e.mjs` | 通过，exit 0 |
| 2 | `PERI_E2E_CONTRACT_SELF_TEST=clean ...` | 通过，exit 0 |
| 3 | `PERI_E2E_CONTRACT_SELF_TEST=gap ...` | **exit 1**，证明 gap 断言失败时子进程非零退出 |
| 4 | 真实栈 e2e | 通过，exit 0，293/293 断言 |
| 5 | `git diff --check` | 通过 |
| 6 | `cargo test -p peri-swarm` | 本次未重跑；本提交只改 `.mjs` 与验收文档，未改 Rust。上一提交同树结果为 351 passed / 0 failed / 6 ignored |

## 9. 清理结果

- 网关与模型 stub 已 kill。
- `docker rm -f peri-loom` 已执行；`peri-loom-data` volume **保留**。
- 既有容器 `peri-cf-peri-loom-clean-hrana` 未触碰。
- 清理后端口：

```text
8080: 空闲
9496: 空闲
9497: OrbStack（Docker Desktop 端口转发；无 Loom 容器）
6420: rivet-eng PID 16504（本次运行启动的 3.1.1 engine，按 README 的共享 engine 约定保留）
```

## 10. 复现

按 §2 启动 Loom（必须使用此前未使用过的全新 run-dir，例如 `/data/peri/run5` 及以后；
复用旧 run-dir 会触发 `owned stop proof set incomplete or unexpected`）、状态库、stub、
网关，然后执行 §3 的驱动命令。不要删除 `peri-loom-data` volume；不要动 6420 上不属于
本次的 Rivet engine。完整 JSON 与原始帧分别由 `PERI_E2E_OUTPUT_PATH`、
`PERI_E2E_RAW_PATH` 写出。
