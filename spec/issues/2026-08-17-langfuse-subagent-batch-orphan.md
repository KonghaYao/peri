# Langfuse subagent 工具批次（batch span）成孤儿：parent stage span 从未发送

**状态**：🔧 已修复（2026-08-17，待实测验证）
**优先级**：高
**类型**：bug
**创建日期**：2026-08-17
**来源**：Langfuse 数据核查（trace `01a00fd279d07743ac0a17d5979c5633`，用户报告 batch `batch_01a00fd3052171d2b6f0e0145dfd1cc0` 下一堆工具调用挂错位置）
**最后核查**：2026-08-17

## 问题描述

Langfuse UI 中 subagent 执行产生的整批工具调用归属错误：工具调用挂在了一个 parent 不存在的 batch span 下（或显示为主 agent 的 batch / 悬浮节点），期望结构为 `subagent AGENT obs → stage → batch → tools`。

## 数据证据

trace `01a00fd279d07743ac0a17d5979c5633`（235 条 observations 全量核查）：

- `batch_01a00fd3052171d2b6f0e0145dfd1cc0`（05:04:42~05:06:26）为 subagent-coder #1 的 tool batch——时间范围与其 AGENT obs `obs_01a00fd2fc4670c3b8155c89b99cc95c`（05:04:40~05:06:31）几乎完全重合；其 parent `span_01a00fd4afa07ed1953d2350b326df8d` **不存在**（API 直查 404）。
- `batch_01a00fd3062d79108273d49740273b29` 同理为 subagent-coder #2 的 batch，parent `span_01a00fdd1fc37813886169ebedb27dce` 也不存在。
- 主 agent 的 batch（`batch_01a00fd2c85b7e639c17862deefabb81`，parent = 主 stage-act）正常。

## 根因

### 第一轮（已修复，覆盖即补发）

stage span 的 SpanCreate **延迟到 `on_stage_end` 合并发送**（v2 条件上报语义，`span_events.rs`）：duration > 0 才上报。subagent 的 reason/act stage 快速切换时，旧 stage 的 StageEnded 可能因乱序/重放丢失（bridge 找不到对应 handle → warn 跳过，`on_stage_end` 不被调用），旧 stage 的 SpanCreate **永不发送**。而工具 batch 在 ToolStart 时已将 parent 冻结为该 stage 的 span_id（`content_owner` → `stages.active_handle`）→ batch 的 parent 引用一个从未创建的 span，整批工具成为孤儿。

### 第二轮实测（trace `01a00fec2bc07ee2acd61db4b6724eff`，2026-08-17 13:38 UTC）

修复后仍有 2 个孤儿 batch（`batch_01a00fee9d197eb0904a16139e03d677` 挂 49 个工具、`batch_01a00fec77767501a0fe5add2220394b` 挂 13 个工具），机制不同：

1. **最后一个 stage 的 StageEnded 事件流截断**：subagent 的最后一个 act stage 创建后（ULID 时间戳证实：batch parent `span_01a00ff16b0` = 13:37:54.945 = 最后一个已发送 reason stage 的结束时刻），subagent 进程在 act 阶段被终止（主 turn 收尾/cancel）→ `run_stage` 的 StageEnded 未发出 → span 永不发送。
2. **batch 在 close 时以该 stage 为 parent flush**：父 Agent 工具 end（`on_invocation_tool_end`）→ 两信号齐备 → `close_subagent` → flush child batch（parent = 未发送的 stage）→ 孤儿。
3. **`on_turn_end` 直接丢弃残留 stage handle**：`self.replayed_stage_handles.clear()`（mod.rs）丢弃未领取的 stage handle，不补发。
4. **`cleanup_turn_end` 只兜底 Active|StopReceived 状态**：Closed 状态 subagent 的残留 tool_batch / 活跃 stage 不处理。

### 第三轮实测（trace `01a01001249e7fc3851f88a0f012491e`，2026-08-17 05:55 UTC）

修复后仍有 2 个孤儿 batch：

- `batch_01a010016df1757082b172ca4644c1ed`：parent `span_01a0100231b67a42b1a3d58e799636dd` 不存在，挂 16 个工具。
- `batch_01a010016e757e3397492c6b297dd778`：parent `span_01a01002c4bc7190a29c29dc24970341` 不存在，挂 29 个工具。

两批分别属于两个并行 `subagent-explorer`。根因是 subagent 的 `ToolBatch` 只在 AGENT obs 关闭时 flush，导致同一个 batch 跨越整个 subagent 生命周期；每次新 Act 开始又通过 `on_act_stage_start` 将该 batch 重挂到最新 Act。最终 parent 落到被 0ms 条件上报过滤的 Act span，整批工具成为孤儿。主 agent 已在每个 Act 结束时 flush，不受此问题影响。

## 修复

### 第一轮

- `peri-controller/src/langfuse/tracer/stages.rs`：`on_stage_start` 返回 `(新 handle, 被覆盖的旧 handle)`，不再静默丢弃。
- `peri-controller/src/langfuse/tracer/span_events.rs`：抽取 `emit_stage_span_close`（0ms / Compact 无工作条件上报逻辑保留）；v1 路径 `on_stage_start` 在覆盖旧 stage 时**立即补发**其合并 SpanCreate。
- `peri-controller/src/langfuse/tracer/subagent_events.rs`：v2 路径 `on_stage_start_gated` 同样覆盖即补发。
- 重复发送无害：乱序 StageEnded 若随后到达，Langfuse 对相同 observation id 是 upsert，最终以较晚的 end_time/status 为准。

### 第二轮（turn end 兜底，防御式终态修复）

- `peri-controller/src/langfuse/tracer/stages.rs`：新增 `take_all_active()` / `take_active()`。
- `peri-controller/src/langfuse/tracer/registry.rs`：`ClosedSubagent` 增加 `agent_id` 字段（3 处构造点）。
- `peri-controller/src/langfuse/tracer/subagent_events.rs`：`emit_subagent_close` 改为 `&mut self`，close 时兜底关闭该 subagent 仍活跃/重放的 stage（stage span 先于 batch flush 发送）；新增 `take_all_replayed()`。
- `peri-controller/src/langfuse/tracer/mod.rs`：`on_turn_end` 不再 clear 丢弃，改为兜底补发**所有**仍活跃的 stage（`take_all_active`）+ 所有未领取的重放 stage（`replayed_stage_handles.drain()`）。turn 结束是终态，此后不可能再有 stage 事件，立即补发安全；乱序 StageEnded 后到按同 id upsert 无害。

### 第三轮（subagent 按 Act 分批）

- `peri-controller/src/langfuse/tracer/span_events.rs`：`on_stage_end` 在每个 `Stage::Act` 结束时按 `agent_id` owner flush 对应 batch；主 agent 使用 `self.tool_batch`，subagent 使用其 registry 内独立 `ToolBatch`。stage span 先发送，再发送 batch/tools，保证父节点先入队。
- subagent close 仍保留残留 batch flush，作为 StageEnded 截断等异常路径的兜底。
- `peri-controller/src/langfuse/tracer/registry_test.rs`：新增两轮 subagent Act 回归测试，断言每个 Act 各有一个 batch，parent 分别指向对应 Act，不跨 Act 合并或漂移。

## 验证

- `cargo test -p peri-controller --lib langfuse`：106 passed, 0 failed（含回归测试 `test_replaced_stage_emits_span_no_orphan_batch`、`test_turn_end_closes_stale_stage_no_orphan_batch`、`test_subagent_flushes_tool_batch_per_act_stage`）。
- `cargo clippy -p peri-controller --all-targets` 通过。
- ⏳ **待实测**：真实会话跑一轮含 subagent 的任务，核查新 trace 中 `subagent AGENT obs → stage → batch → tools` 归属链完整、无孤儿 batch。

## 残余窗口（未修，防御性说明）

- **0ms stage 跳过**：stage 被覆盖时若补发时 duration 仍为 0ms，span 仍不发送（v2 条件上报语义保留）。实际场景中覆盖前已有工具调用（>0ms），理论风险低。
- **stage 链式 parent**：StageEnded 丢失时，新 stage 会挂到旧 stage 下（链）而非 AGENT obs 的 sibling——归属链完整无孤儿，但层级与正常路径不同，接受。
- **mq_counts 丢失**：被覆盖的 Receive stage 补发时其 mq 计数无法取回（active 已被新 stage 替换），input 为 None。Receive 被快速覆盖属罕见场景，暂不处理。

## 涉及文件

- `peri-controller/src/langfuse/tracer/stages.rs` —— `on_stage_start` 返回被覆盖 handle；`take_all_active` / `take_active`。
- `peri-controller/src/langfuse/tracer/span_events.rs` —— 抽取 `emit_stage_span_close` + v1 覆盖即补发。
- `peri-controller/src/langfuse/tracer/subagent_events.rs` —— v2 覆盖即补发；`emit_subagent_close` 兜底关闭活跃 stage。
- `peri-controller/src/langfuse/tracer/registry.rs` —— `ClosedSubagent.agent_id`。
- `peri-controller/src/langfuse/tracer/mod.rs` —— `on_turn_end` 兜底补发所有残留 stage。
- `peri-controller/src/langfuse/tracer/registry_test.rs` —— 回归测试 ×2。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-17 | — | 已修复（待实测） | agent | 数据核查定位孤儿 batch → 覆盖即补发修复 + 回归测试 |
| 2026-08-17 | 已修复（待实测） | 已修复（第二轮，待实测） | agent | 实测 trace 01a00fec2b... 仍 2 个孤儿 → 定位"最后 stage StageEnded 截断 + turn end 丢弃残留"→ 三层兜底 |
| 2026-08-17 | 已修复（第二轮，待实测） | 已修复（第三轮，待实测） | agent | 实测 trace 01a01001249e... 仍 2 个孤儿 → 定位 subagent batch 跨 Act 累积并漂移到 0ms Act → 改为每个 Act 结束时独立 flush |

## 修复记录

| 日期 | commit | 说明 |
|------|--------|------|
| 2026-08-17 | 待提交 | 修复 + 回归测试（两轮） |
