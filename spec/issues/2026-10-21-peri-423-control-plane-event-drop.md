# P1：控制面事件通道静默丢弃事件（GitLab issue #3）

状态：根因已复核，修复见 PR #264，等待合入。日期：2026-10-21。

- 原始 issue：<https://gitlab.pazhoulab-huangpu.com/aos-test/peri-test/-/issues/3>
- 原始报告原文：[`2026-10-21-peri-423-control-plane-event-drop-original-report.md`](2026-10-21-peri-423-control-plane-event-drop-original-report.md)
- 证据目录：`bugs/2026-10-21-peri-423-event-drop/`（GitLab `main`，含 README 与 8 份证据）
- 被测版本：`agent-v4.2.3`
- 复现方式：DeepSWE 单题 Docker；`peri -p` + StreamJson；OpenAI 兼容 → LiteLLM → `deepseek-v4-flash`

> 本文保留原始报告的事实、数字与推断，并单独标注「源码复核」结论。原始报告明确声明未读源码；其推断与本次复核不一致处以后者为准。

## 1. 原始结论（TL;DR）

1. 单题 10 分 33 秒内出现 **13,271 条** `WARN peri_controller::controller: 控制面弹出队列事件未投递 session_id=… outcome=Dropped`，占 13,311 行日志的 **99.70%**。
2. 告警只带 `session_id` 与 `outcome`，不带事件类型、生产者、容量/水位或累计丢弃数，事后无法判断丢失的事件类别。
3. 收尾阶段另有 **3 个后台任务回执**等待满 120 秒后被放弃，随后 0.31 秒 / 1.90 秒才到达。
4. 本次运行未判为异常：`exception_info=null`、exit 0、`session_close_failures: 0`；题目失败属能力问题。
5. 与 #2 的关系：#2 的 ACP 收尾失败在 4.2.3 已修复；本问题是同一事件通道上的另一个问题。

## 2. 原始现象

### 2.1 日志规模

| 指标 | 值 |
|---|---|
| 日志 | 13,311 行 / 2,183,437 字节 |
| WARN / ERROR | 13,293 / 0 |
| `控制面弹出队列事件未投递 … outcome=Dropped` | 13,271 条 |
| 占比 | 99.70% |
| 第一条 | `06:50:39.574` |
| 最后一条 | `07:01:12.996` |

除该告警外，整份 11 分钟日志只剩约 40 行有效信息。

### 2.2 三段持续过载

| 区段 | 条数 | 起 ~ 止 | 持续 | 速率 |
|---|---:|---|---:|---:|
| 1 | 11,319 | 06:50:39.574 ~ 06:52:21.335 | 102 秒 | 约 111 条/秒 |
| 2 | 732 | 06:52:21.336 ~ 06:54:52.112 | 151 秒 | 约 4.8 条/秒 |
| 3 | 1,217 | 06:54:52.113 ~ 06:59:12.987 | 261 秒 | 约 4.7 条/秒 |
| 4 | 1 | 07:01:12.994 | — | — |
| 5 | 2 | 07:01:12.995 ~ 07:01:12.996 | — | — |
| 合计 | 13,271 | 跨度 633 秒 | 占空比 19% | 均值约 21 条/秒 |

单秒最多 380 条。最长静默间隔为 **122.85 / 120.10 / 120.04 / 120.01 秒**，与收尾 `waited_ms=120000` 接近，原始报告因此推测存在约 120 秒的周期边界。

### 2.3 告警字段

```text
session_id=01a15d39-7c84-7ca2-adac-95f6892ed45f  outcome=Dropped
```

原始报告指出缺失：事件类型/kind、生产者模块、队列容量与当前水位、该类型累计丢弃数、丢弃序号。

### 2.4 后台回执

```text
07:01:12.991  WARN peri_agent::agent::async_tasks::handoff:
              bounded wait expired; pending task handoff recorded
              pending=3 waited_ms=120000
07:01:13.304  result_accepted  → 放弃后 0.31 秒
07:01:14.896  result_accepted  → 放弃后 1.90 秒
```

三个回执均标记 `agent_name=mcp success=false output_len=94`。

### 2.5 判分结果

```json
"agent_info": {"name": "peri", "version": "agent-v4.2.3"},
"exception_info": null,
"verifier_result": {"rewards": {"reward": 0, "f2p": 0.0, "p2p": 1.0,
                                "f2p_total": 2, "f2p_passed": 0,
                                "p2p_total": 214, "p2p_passed": 214}}
```

同题 4.1.1 也是 `f2p 0/2`、`p2p 214/214`，但伴随异常；4.2.3 结果相同且异常消失。

## 3. 原始关键日志与时间线

```text
06:49:09  trial 开始
06:50:10  peri print 模式启动（provider=OpenAI）
06:50:10  WARN rmcp::service: -32602 "missing task scope capability"
06:50:17  WARN 订阅流错误，重试耗尽，停止消费 server=workspace
   ↓ 21.6 秒
06:50:39  【第 1 条 Dropped】开始 102 秒、约 111 条/秒的丢弃风暴
06:52:21  WARN workspace text read failed (-32603)
06:54:52  WARN workspace text read failed (-32603)
06:59:12  最后一次大批量 Dropped
06:59:12  handoff 开始等待 3 个后台任务（由 120 秒超时倒推）
07:01:12  WARN bounded wait expired; pending=3 waited_ms=120000
07:01:13  WARN 状态层事件未投递 event="state_event_dropped" outcome=Closed
07:01:14  再 2 条 state_event_dropped outcome=Closed
07:01:14  INFO ACP host terminal shutdown complete，session_close_failures: 0
07:10:39  trial 结束
```

原始报告已排除的外部因素：

1. 不是网络：网关 52 次模型调用全部 success，0 失败。
2. 不是模型：输出流无 `is_error`，51 个 assistant 轮次正常结束。
3. 不是工具结果丢失：80 个工具调用与 80 个工具结果一一配对。
4. 与模型输出量强相关：13,271 条丢弃 / 29,228 output tokens ≈ 0.45 条/token；丢弃最多的分钟正是输出最多的分钟。

## 4. 原始报告的影响面

1. 99.7% 日志被同一告警占满，真实故障信息被淹没。
2. 若被丢弃的是工具结果、任务完成、会话关闭或生命周期事件，可能复现 #2 类假失败；本次无法排除，因为日志不记录事件类型。
3. 可确认损失：至少 3 个后台任务回执因超时被放弃（任务本身 `success=false`）。
4. 4.1.1 全量 115 份日志中 `peri_controller::controller` 为 0 行；但 `state_event_dropped` 出现在 114/115 份，合计 342 次。4.2.3 同题为 controller 层 13,271 次 + bus 层 3 次。
5. 两版触发条件相同（`-32602` → 订阅流停止消费），但 4.1.1 停摆后 9 分钟无 controller 丢弃，4.2.3 停摆后 21.6 秒开始丢弃并持续约 8.5 分钟。
6. 日志措辞从 4.1.1 的「状态层通道已满，事件丢弃」变为 4.2.3 的「…未投递 … outcome=…」，并新增 `peri_controller::controller` 生产者；原始报告判断这可能是 4.2.3 改动引入，但单题单次运行不足以定性。

## 5. 原始建议方向

1. **P1**：生命周期/工具结果/close 等关键事件不得丢弃，使用可靠通道或阻塞背压。
2. **P1**：丢弃告警记录事件 kind、生产者模块、容量/水位、按 kind 累计计数。
3. **P2**：告警限流/聚合（首次打印 + 累计计数，或每秒至多一条）。
4. **P2**：排查通道容量与消费者生命周期，重点确认 workspace MCP 订阅消费者退出后事件是否仍往同一通道投递。
5. **P3**：workspace MCP `-32602 missing task scope capability`；若确认是触发条件，应优先修复。

## 6. 源码复核（本次修复依据）

复核基线：`7a04c8a70`；4.2.3 对应 `agent-v4.2.3` 的 `peri-controller/src/controller.rs` 与 HEAD 相同。

### 6.1 根因

`Controller` 自 3.0 起在 `controller.rs` 内持有一个容量 `1024` 的 mpsc 队列：

```rust
let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
```

`publish_message` 先 `try_send` 到该队列，失败即记录 `Dropped`/`Closed` 并打 WARN，然后再独立 `broadcast::send` 给订阅者。问题在于：

1. `pop_events()` 在整个仓库中**只有 `controller_test.rs` 调用**；生产代码没有消费者。
2. ACP、executor event pump、Langfuse 旁路实际都经 `Controller::subscribe()` 的 broadcast 消费。
3. `events_rx` 一直由 Controller 自己持有且无人排空，所以第 1025 条起每次 `try_send` 都返回 `Full`，每条事件打一条 WARN，永久持续。

因此报告的 13,271 条告警是**一个无人消费的影子队列**在丢克隆副本；ACP 实际消费的广播分支不受影响。报告担心的「关键事件被这条通道静默丢掉」在本路径上不成立。但这不构成端到端可靠交付保证：广播慢消费者仍会 lag，只是会显式返回 `Lagged(n)`。

4.1.1 的同一队列写作 `let _ = self.events_tx.try_send(msg.clone());`，没有健康度记录与 WARN；PR #248（2026-10-20）新增了记录和告警，所以看起来像 4.2.3 的回归，实际是**废队列丢弃第一次被记录**。

### 6.2 与原始报告两条线索的关系

1. workspace MCP 订阅停止消费：走 `McpClientPool` 的 subscription/task scope 路径，不经过 Controller mpsc；停止它不会让该队列「少一个消费者」，因为它本来就没有消费者。
2. 3 个后台回执 120 秒超时：走 `peer.get_task` → `TaskManager::settle_external` → `AsyncResultMailbox` / `MessageQueue` → handoff 路径，也不经过该队列。报告「事件被压在通道里」的推断未获源码支持；回执迟到原因仍需独立调查。

### 6.3 未解决项

1. workspace MCP `-32602 missing task scope capability`（报告 P3）。
2. 3 个后台回执等待 120 秒后放弃、随后 0.31s/1.90s 到达的 handoff 时序。
3. 源端 EventBus 的 `state_event_dropped outcome=Closed`：state 通道是有界 mpsc，`emit_state` 在满或接收端关闭时丢弃并告警；本次 3 条是 forwarder 退出后 `Closed`，4.1.1 同样存在，非本次引入。
4. 可靠交付契约仍不完整：Controller 广播的慢消费者收到 `Lagged(n)`，只报告错过条数、不重放；EventBus observe 通道 lag 只记 WARN 不补发；render/state 普通投递满即丢，仅 `emit_render_reliable` / `emit_state_reliable` 阻塞等待。哪些事件必须走可靠路径仍待单独设计。

## 7. 修复

PR：<https://github.com/KonghaYao/peri/pull/264>

1. 删除无消费者的 mpsc 弹出队列、`Controller::pop_events`，以及只服务于该队列的 `bind_event_health` / `event_health` 记账。
2. `publish_message` 只投递订阅广播；慢消费者仍经 `SubscriptionError::Lagged` 显式报告。
3. 新增独立集成回归测试 `peri-controller/tests/controller_event_delivery.rs`：全局 tracing subscriber，避免 callsite 兴趣缓存在并行测试中造成的空洞通过；旧实现稳定复现告警，修复后通过。
4. 同步 `docs/design/architecture.md`、`docs/code-index/` 与 ACP 注释。

## 8. 验证

1. `cargo test -p peri-controller --lib`：181 passed。
2. `cargo test -p peri-controller --test controller_event_delivery`：1 passed；在 HEAD 旧实现上 FAILED，捕获 1024 条后的 `Dropped` 告警。
3. `cargo test -p peri-agent --lib -- event_pump`：15 passed。
4. `cargo test -p peri-acp --lib -- host::prompt`：20 passed。
5. `bash scripts/check-changed.sh`、`cargo fmt --all -- --check`、pre-commit（check/clippy/fmt/layer-imports）通过。

## 9. 证据文件（原始报告）

| 文件 | 内容 |
|---|---|
| `peri-print.2026-10-21.log` | 脱敏 + 折叠版内部日志（连续重复折叠并注明条数与起止） |
| `warning-summary.txt` | 按告警类型/区段/分钟的统计及字段说明 |
| `timeline.txt` | 时间线与 4.1.1 同题对照 |
| `result.json` | pier 判分结果 |
| `peri-result-line.txt` | 输出流末条 result 与 token 用量 |
| `peri-print.2026-10-20.v411-same-task.log` | 4.1.1 同题完整日志（59 行） |
| `arm-comparison-411-vs-423.txt` | 4.1.1 全量 115 份日志检索统计与逐项对照 |
| `cause-elimination.txt` | 网关 52/52、工具 80/80、逐分钟相关性 |

原始完整日志（2.18 MB）留在评测机 trial 目录；仓库只收录折叠版。所有证据来自同一次真实运行，密钥类内容扫描 0 命中。
