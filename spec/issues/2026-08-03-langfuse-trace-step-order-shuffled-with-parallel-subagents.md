# Langfuse trace 中并行 subagent 的 step 顺序错乱，step-28 被渲染为最后一个

**状态**：Open
**优先级**：中
**创建日期**：2026-08-03

## 问题描述

Langfuse UI 中查看 trace `019fc718102871a39b4df6334f1d596e`（turn 019fc718-1028-71a3-9b4d-f6334f1d596e）时，step 顺序错乱：step 编号重复出现（step-28 出现 3 次）、step 编号与时间顺序不一致，且 step-28 被渲染成整条 trace 的最后一个 step，无法按时间线正常阅读 agent 执行过程。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| step 编号全局重复 | step-28 出现 3 次（02:10:00 / 02:10:28 / 02:11:16）；step-6/7/8 等多编号重复 |
| step 编号与时间交错 | 02:08:31 起出现新的 step-1..step-36 序列（此时主 agent 已到 step-6），与主 agent 序列交错 |
| step-28 被渲染为最后一个 | subagent-3（peri-middlewares）的最后一步 LLM 调用是 step-28（02:11:16.935），其 stage-reason span 与 subagent-3 的 AGENT observation 互相引用成环（A→B→A），UI 树渲染错乱 |
| 单个 trace 混入多个 agent 的执行 | 1 个主 agent + 4 个并行 subagent（02:08:31 同一 tool-batch 派发，explore peri-agent / peri-acp / peri-middlewares 等）全部写入同一 trace |
| 大量 stage span 挂在缺失的父节点下 | 215 个 span 挂在 `obs_...d232f2c8481` 下，但该 observation 本体不存在（4 个 Agent 工具调用只创建了 3 个 AGENT observation） |
| subagent 的 LLM 调用多数未挂到对应 AGENT obs 下 | subagent-1 仅 2 个 generation 挂在自身子树（step-35/36），其余丢失或挂错 |

## 复现条件

- **复现频率**：必现（并行 subagent 场景）
- **触发步骤**：
  1. 主 agent 通过一个 tool-batch 并行派发多个 subagent（如 Agent 工具多次调用）
  2. 各 subagent 执行多轮 ReAct 循环（多步 LLM 调用）
  3. 在 Langfuse UI 查看该 trace
- **环境**：peri-acp langfuse tracer v0.2.0，deepseek-v4-flash，本地 langfuse（localhost:23332）

## 涉及文件

- `peri-acp/src/langfuse/tracer/stages.rs` —— ReAct stage span 生命周期管理（单 active slot）
- `peri-acp/src/langfuse/tracer/mod.rs` —— subagent AGENT observation 创建时 parent 取活跃 stage span
- `peri-acp/src/langfuse/bridge.rs` —— StageStarted 事件映射，stage span parent 取 subagent 栈顶
- `peri-agent/src/session/turn.rs` —— step 计数器（per-turn，subagent 从 1 重新计数）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-03 | — | Open | agent | 创建 |
| 2026-08-03 | Open | In Progress | agent | 定位根因：bridge 共享 active_stage 单槽 + generation 缓存无 agent 隔离 + AGENT obs parent 循环引用 |
| 2026-08-03 | In Progress | Fixed | agent | 修复完成，测试通过（见修复记录） |

## 修复记录

### 修复 #1（2026-08-03）

- **操作人**：agent
- **用户原意**：Langfuse trace 中并行 subagent 的 step 编号不重复、顺序正确，能按时间线正常阅读。
- **修复内容**（三个根因，全部按事件自带 agent_id 隔离）：
  - **generation 缓存隔离**（`generation.rs`）：`generation_data` key 从 `step` 改为 `(agent_id, step)`，`on_llm_start` / `on_llm_request_payload` / `on_llm_end` 签名加 `agent_id` 参数——并行 subagent 各自独立 step 计数器不再互相覆盖。
  - **stage span slot 隔离**（`stages.rs`）：`active: Option<ActiveStage>` 改为 `active: HashMap<String, ActiveStage>`（key = agent_id），`on_stage_start` 只覆盖同一 agent 的前一个 slot，`on_stage_end` 校验 handle 匹配才清理，新增 `active_stage(agent_id)` / `active_handle(agent_id)`；`MAIN_AGENT_KEY = "main"` 用于 v1（workflow）路径。
  - **循环引用修复**（`mod.rs` + `subagent.rs`）：AGENT observation 的 parent 从"结束时刻的活跃 stage span"改为 `begin_subagent` 时捕获的 parent（`SubAgentContext.parent_observation_id`），杜绝并行场景 A→B→A 成环。
  - **bridge 层**（`bridge.rs` + `forwarder.rs`）：`UnifiedLangfuseEvent` 的 LLM/Stage/工具事件加 `agent_id` 字段（v1 ExecutorEvent 路径回退 `"main"` / source_agent_id），`LangfuseBridge.active_stage` 改为 `HashMap<String, StageHandle>`，`StageEnded` 按 agent_id 精确配对。
- **验证状态**：已验证 —— `cargo test -p peri-acp` 409 个测试全过；`cargo clippy -p peri-acp --all-targets -- -D warnings` 通过；`cargo check --workspace` 通过。Langfuse UI 真实验证待用户下次跑并行 subagent 场景确认。
- **已知限制**（未修，需架构改动）：4 个并行 subagent 中第 4 个的 AGENT observation 缺失、215 个 span 悬挂其下——根因是 `ObserveEvent::SubagentStart` 无生产 emit 处，无法建立 AgentId → observation_id 映射；subagent 的 stage span 归属 AGENT obs 仍需按栈顶近似。
