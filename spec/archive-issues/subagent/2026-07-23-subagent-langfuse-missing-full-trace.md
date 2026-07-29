# Subagent Langfuse trace 缺少完整的 trace 结构——与主 agent 不等价

**状态**：Archived
**优先级**：中
**创建日期**：2026-07-23
**类型**：技术债

## 问题描述

Subagent 在 Langfuse 上的 trace 结构与主 agent 不等价。主 agent 有完整的 turn → stage span → tool observation / generation 层级，但 subagent 仅产生一条父 agent 的 Agent 工具 observation——subagent 内部的 Read/Write/Bash 等工具调用、LLM 调用、compact 阶段在 Langfuse 上完全不可见。

## 现状

**主 agent trace 结构**（`executor_helpers.rs:459`）：
```
trace
 ├─ stage-compact span（CompactStarted/Completed）
 ├─ stage-reason span（Reason）
 │   └─ generation（LlmCallStart/End → token 统计）
 ├─ stage-act span（Act）
 │   ├─ tool-Bash observation（input/output/耗时）
 │   ├─ tool-Read observation
 │   └─ tool-Agent observation（→ subagent，仅 input/output，无内部结构）
 └─ stage-end span
```

**Subagent trace 结构**（`execute_fork.rs:146`）：
```
（空——没有任何 Langfuse trace 产出）
```

父 agent 通过 `tracer.on_tool_start/on_tool_end` 嗅探 Agent/Task 工具名来间接记录 subagent 的 start/stop 边界，但这只产生一条工具级 observation，无法展开 subagent 内部细节。

## 涉及文件

- `peri-agent/src/agent/subagent_event_forwarder.rs` —— subagent 专用事件转发器，消费 render/state/observe 三通道但**无 LangfuseBridge 参数**，只做 TUI 转发
- `peri-acp/src/event/forwarder.rs` —— `spawn_eventbus_forwarder` 是主 agent 的转发器，**有 LangfuseBridge 参数**，forwarder loop 内对每类事件调 `bridge.process_event()`
- `peri-middlewares/src/subagent/tool/execute_fork.rs:146` —— fork subagent 调用 `spawn_subagent_event_forwarder`（无 Langfuse 能力）
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— bg subagent 同上
- `peri-acp/src/langfuse/bridge.rs:640-642` —— `SubagentStart/SubagentStop` 在 bridge 中被显式跳过（"暂无 Langfuse 映射，静默跳过"）
- `peri-acp/src/langfuse/tracer/subagent.rs` —— `SubagentStack` 已存在，但仅用于父 agent 工具嗅探压栈，无法承载独立 trace

## 期望改进方向

Subagent 的 `spawn_subagent_event_forwarder` 对齐 `spawn_eventbus_forwarder`，增加 `bridge: Option<LangfuseBridge>` 参数，在 forward loop 内对 render/observe 事件调用 `bridge.process_event()`。这样 subagent 能自动获得与主 agent 相同的 trace 结构：stage span → tool observation / generation → compact span。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-23 | — | Open | agent | 创建——分析 subagent Langfuse trace 缺口时发现 |
| 2026-07-23 | Open | Fixed | agent | 修复：subagent forwarder 增加 LangfuseBridge 支持，11 文件 115+ 行 |
| 2026-07-23 | Fixed | Reopen | agent | 线上验证：14 个 subagent observation 均为 0 子节点，修复未生效。Langfuse OTLP 手工测试确认 API 侧完全支持嵌套 AGENT 结构，问题在 peri 代码侧 |
| 2026-07-23 | Reopen | Fixed | agent | 修复 3 个 P0 BUG：(1) biased select! 时序 (2) bg subagent 栈时序 (3) 共享 tool_batch。共 9 文件、16 个新测试，951 tests pass |
| 2026-07-25 | — | Archived | agent | 归档

## 修复记录

### 修复 #1（2026-07-23）

- **操作人**：agent（auto-devflow: explore → plan → code → review）
- **用户原意**：subagent 和主 agent 拥有同样的 Langfuse trace 结构
- **修复内容**：
  - 新建 `peri-agent/src/agent/langfuse_bridge.rs`：定义 `LangfuseBridgeLike` trait（解耦 peri-agent 与 peri-acp 的跨层依赖）
  - 修改 `spawn_subagent_event_forwarder`：增加 `bridge: Option<Arc<dyn LangfuseBridgeLike>>` 参数，render/observe 分支在 ev 被 move 前调用 bridge
  - 修改 `peri-acp/src/langfuse/bridge.rs`：`LangfuseBridge` impl trait，`active_stage` 内部化管理（`Arc<Mutex<Option<StageHandle>>>`），`StageStarted` 的 `parent_observation_id` 走 `current_agent_id()`
  - 修改 `SubAgentTool` + `SubAgentMiddleware`：增加 `langfuse_bridge` 字段 + builder + `build_tool()` 透传
  - 修改 5 个调用点（fork/bg/define/spawner/bg_command）：传递 bridge
  - 修改 `builder.rs`：从 `langfuse_tracer` 构造 `LangfuseBridge` 注入 `SubAgentMiddleware`
- **涉及文件**：11 个（+1 新建）
- **验证状态**：已验证（build ✅ / peri-agent 635 ✅ / peri-acp 299 ✅ / peri-middlewares 1059 ✅）
- **审查结论**：PASS——架构正确，向后兼容。已知限制：并发 bg subagent 共享 `active_stage`（stage span 交错可能覆盖，后续优化）

### 验证 #1（2026-07-23）—— Reopen

**线上数据验证**（2026-07-23 12:29~13:00）：
- 搜索 Langfuse 项目 `cmqjich7n004cad0dkd1xf942` 中最近 3 天全部 14 个 subagent observation
- **所有 14 个 subagent 均为 0 子节点**（包括 11 个后台 + 3 个同步）
- subagent 的 thread session 均有 0 个 trace
- 主 agent 的 `agent-run` 正常工作（32 个子节点）

**Langfuse API 能力验证**（OTLP 手工测试）：
- 通过 OTLP 端点手工注入嵌套 AGENT 结构
- 3 层 AGENT 嵌套（agent-run → subagent → sub-subagent）全部正常
- subagent 下挂载所有类型节点（SPAN/GENERATION/EVENT/TOOL/COMPACT）全部正确
- 结论：**Langfuse OTLP 完全支持嵌套 AGENT 结构，问题 100% 在 peri 代码侧**

**关键对比**：

| 项目 | 手工 OTLP 测试 | 真实 peri 数据 |
|------|:---:|:---:|
| agent-run 类型 | AGENT ✅ | AGENT ✅ |
| agent-run 子节点 | 4 ✅ | 32 ✅ |
| subagent 类型 | AGENT ✅ | AGENT ✅ |
| subagent 子节点 | **2** ✅ | **0** ❌ |

**注意**：OTLP `langfuse.observation.type` 必须用小写（`"agent"`），大写会被 Langfuse 默认为 SPAN（`conversion.rs:306-308` 做了 `to_lowercase()`）。
