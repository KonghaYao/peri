> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-langfuse-subagent-attribution-stack-lifetime.md

# Langfuse 上报中 subagent 内容整体错挂到主 agent：SubagentStack 生命周期与事件消费时序错配

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-05

## 问题描述

Langfuse trace `019fccee3ee872b19943058eca44eae5`（turn 019fccee-3ee8-72b1-9943-058eca44eae5，2026-08-04 05:19:53 开始，user=KonghaYao）中，两个 subagent 的内部执行内容（stage span、generation、tool）**全部**直接挂在主 agent 的 `agent-run` observation 下，与主 agent 自身的 ReAct 循环平铺交错；两个 subagent 的 AGENT observation 是 17ms/19ms 的空壳。Langfuse UI 无法按 agent 维度阅读该 trace。

## 症状详情（trace 数据证据）

| 现象 | 数据证据 |
|------|---------|
| 全部 stage span 错挂 agent-run | 261 个 observation 中 127 个 `stage-*` span 的 `parentObservationId` 全部是 agent-run 的 id（`019fccee3ee872b199430593bd8de722`），包括两个 subagent 运行期间创建的 |
| subagent AGENT observation 空壳 | `obs_019fccef32d5778091a5b199ace9ebbb`（subagent1，05:20:56.021→.038，**17ms**）、`obs_019fccf0b5f471409cbf456f4acffa49`（subagent2，05:22:35.124→.143，**19ms**），0 个子节点；parent 是主 agent 的 stage-act span（此点正确） |
| AGENT obs 生命周期 ≈ 工具调用 | 两个 AGENT obs 的 start/end 与对应 TOOL Agent（主 agent 的工具调用）完全相同 —— AGENT obs 不是"subagent 执行周期"的镜像，而是"入栈→弹栈"瞬间的产物 |
| step 编号时间线乱序 | 主 agent step-1..25（in 13.4k→127k 单调）、subagent1 step-1..12（8.9k→48.9k）、subagent2 step-1..8 平铺于 agent-run 下，时间线出现 4 次编号回退（step-5→step-1、step-10→step-9→step-1、step-13→step-4、step-25→step-11） |
| 幽灵序列 | 05:28:16 出现 `step-11 in=76732` / `step-12 in=76907`，无对应 AGENT observation；subagent2 窗口内还混入 `step-9 in=63232`（05:22:35.159，CF Worker 主题，与主 agent 的 sync.md 主题不同源）——弹栈后晚到的残留事件 |
| 正确挂载数 = 0 | 没有任何 observation 的 parent 指向两个 subagent 的 obs id |

## 毫秒级时间线（subagent1，两个 subagent 同构）

| 时刻 | 事件 | 栈状态 |
|------|------|--------|
| 05:20:56.021 | 主 agent `ToolStarted(Agent)` → `begin_subagent`，AGENT obs start 记录 | `[#1]` |
| 05:20:56.038 | 主 agent `ToolEnded(Agent)` → **栈被弹出**，AGENT obs 创建（end=.038） | **空** |
| 05:20:56.047 | 主 agent 继续 reason（step-5） | 空 |
| 05:20:56.054 | **subagent1 第一个 stage-reason 才创建** → `current_agent_id()` fallback 主 agent → parent=agent-run | 空 |

subagent2 同构（.124→.143 弹栈，第一个 stage span .158 创建）。**栈在 subagent 产生任何内容之前 9~15ms 就被弹出**，之后该 subagent 的所有事件全部静默 fallback 到主 agent。

## 复现条件

- **复现频率**：非并行（顺序）subagent 场景下同样发生 —— 本 trace 两个 subagent 顺序执行（间隔 103ms，无重叠），依旧全部错挂
- **触发步骤**：
  1. 主 agent 调用 Agent 工具（subagent 执行多轮 ReAct）
  2. 在 Langfuse UI 查看该 trace
- **环境**：peri-acp langfuse tracer v0.2.0（08-03 修复后），本地 langfuse（localhost:23332）

## 涉及文件

- `peri-acp/src/langfuse/tracer/subagent.rs` —— `SubagentStack`：纯 LIFO 栈顶操作（`current_agent_id` / `mark_top_started` / `top_has_started` / `end_subagent`），无 agent_id 索引
- `peri-acp/src/langfuse/tracer/mod.rs` —— `on_tool_start:704`（begin）、`on_tool_end:743-796`（fork/bg 判定 + 弹栈）、`on_llm_start:408`（无条件 mark 栈顶）、`on_turn_end:216-257`（清理）
- `peri-acp/src/langfuse/bridge.rs` —— `StageStarted:721-749`：stage span parent 取 `current_agent_id()`、无条件 `mark_top_started()`（不校验事件 agent_id 与栈顶对应）
- `peri-agent/src/agent/subagent_event_forwarder.rs` / `peri-acp/src/event/forwarder.rs` —— 两个独立 tokio task 消费事件，顺序无保证
- `peri-middlewares/src/subagent/v2_bridge.rs` —— subagent 独立 session / EventBus / AgentId（事件侧有完整 agent_id）

## 根因分析（2026-08-05 三路并行调查：trace 数据反推 + 事件路径追踪 + 栈状态机审计，结论交叉一致）

1. **`top_has_started()` / `mark_top_started()` 无 agent 校验**：任何 agent 的 `StageStarted` / `LlmCallStart` 都无条件把**栈顶** subagent 标记为 started（`bridge.rs:739`、`tracer/mod.rs:408`），`on_tool_end` 弹栈时不校验 `tool_call_id` 与栈内 context 的对应关系。栈顶标志被无关事件污染，导致栈在 subagent 实际工作前被弹出。
2. **跨 forwarder 竞态（结构性的，非小概率）**：主 forwarder（render 优先）与 subagent forwarder（observe 优先）是两个独立 tokio task，共享 `Arc<Mutex<LangfuseTracer>>`（`bridge.rs:557`）；`ToolEnded`（主 forwarder 消费）与 subagent 首个 `StageStarted`（subagent forwarder 消费）的相对顺序由调度决定，`tool_dispatch.rs:308` 的 `yield_now()` 仅为 best-effort。本 trace 中两者相差 9~15ms，ToolEnded 先到 → 栈空 → 后续全部 fallback。
3. **08-03 已知限制成为实际根因**：`ObserveEvent::SubagentStart/SubagentStop` 无生产 emit 处（全仓仅测试文件有），tracer 无法建立 AgentId → observation_id 映射，subagent 内容归属只能"按栈顶近似"。**本 trace 是 08-03 修复后的首个真实 subagent 验证 —— 验证失败**：修复解决了 step 缓存覆盖（`(agent_id, step)` key）与循环引用（AGENT obs parent 提前捕获），但归属错乱是另一个独立的结构性缺陷，且**顺序 subagent 同样触发**（当时 spec 的"并行场景"假设过窄）。

## 与历史 issue 的关系

- `2026-08-03-langfuse-trace-step-order-shuffled-with-parallel-subagents.md`：同域问题（step 乱序），08-03 修复了 generation 缓存隔离 / stage slot 隔离 / 循环引用，状态 Fixed；其"已知限制"（SubagentStart 无 emit、归属按栈顶近似、需架构改动）在本 trace 中被证实为**未解决的根因**，且影响面更大（顺序 subagent 也错挂）。本 issue 是该限制的架构级跟进。

## 架构结论：需要重构，补丁不可行

**是。** 需要从架构视角重构 subagent 内容归属机制，理由：

1. **事件侧已有完整身份，栈侧没有**：08-03 修复已给所有事件加了 `agent_id`（subagent 自带 UUID v7），但 `SubagentStack` 仍是无身份的 LIFO 近似，二者之间唯一的桥梁是"栈顶"。`SubAgentContext.agent_id`（`agent_xxx`）与事件 agent_id（session.agent_id）从无映射关系（`subagent.rs:12-28`）。
2. **栈生命周期与事件消费耦合在错误的位置**：入栈由主 forwarder 的工具事件驱动，出栈由主 forwarder 的 ToolEnded 时序决定，而内容事件由 subagent forwarder 消费 —— 三者在两个无同步的 task 中交错，任何补丁式的时序调整（yield_now、加锁顺序）都无法消除结构性竞态。
3. **重构方向**：2026-08-05 经 advisor（Opus）咨询定稿，见下节"重构方案"。

## 重构方案（advisor 咨询定稿，2026-08-05）

**结论：A+B+C 组合 —— 用"身份注册表"替代"生命周期栈"。** 推荐理由（advisor 评估）：单独 A（映射）无法解决"何时创建/关闭 AGENT obs"与 ToolEnded 竞态；单独 B（tool_call_id）只能修复 Agent tool 结束歧义，无法路由 child 的 stage/generation/tool；单独 C（完成信号）不解决并行/嵌套归属。三者必须组合，且这是最小侵入的增量方向：保留 08-03 已正确的 `active_stage`（HashMap<agent_id>）、generation `(agent_id, step)` 隔离与 AGENT obs parent 提前捕获，**仅替换"栈顶推断"层**。

### 状态结构（替代 `SubagentStack` 的两张表）

1. **`subagents_by_agent_id: HashMap<事件侧AgentId, ActiveSubagent>`** —— 内容归属
   - value：`observation_id`（AGENT obs id）、`parent_observation_id`（注册时冻结的父 stage obs id）、`start_time`、`invocation_key`、child 所属 ToolBatch 引用、completion 状态（Stop 时间 / 是否已关闭）、deferred_output 关联
   - `observation_id` 必须在 child 第一个 stage/generation/tool 实际入队前创建（AGENT start observation 先发或与 child 同批提交）
2. **`subagent_invocations: HashMap<(父AgentId, ToolCallId), SubagentInvocation>`** —— 调用关联（B）
   - value：调用时捕获的 parent stage obs、Agent tool 的 input / ToolBatch 位置 / 结束状态、已绑定的 child_agent_id（Start 到达前可为空）、deferred_output、是否已接收 child Stop
   - 所有 tool lookup 改用 owner agent identity + tool_call_id；**移除"所有层 ToolBatch 搜索 tool_call_id"的归属判定**（`is_agent_tool_anywhere`）

### 事件契约（恢复生产 emit，新增字段不改旧字段）

`ObserveEvent::SubagentStart` / `SubagentStop` 字段已完备（`events_v2.rs:342-357`：child_agent_id / 父 agent_id / agent_name / is_background / result / is_error），**仅缺生产 emit**：
- `SubagentStart` emit 时机：child AgentId 已分配、父 Agent tool invocation 已确定、child 首个 ReAct 事件不可能产生之前 —— 即 `execute_fork.rs:110` / `execute_bg.rs:153` 现有 `SubagentStarted { instance_id }` 的 emit 点（已确认在生产中、与 ToolStart 的先后由 `tool_dispatch.rs:304-308` 的 yield_now 保证）
- `SubagentStop` emit 时机：child `run_react_loop` 的 success/error/cancel 所有退出路径，经 finally/RAII 语义

### 事件流（8 步状态机）

1. 父 `ToolStart(Agent tool)` → 记录 `SubagentInvocation`（冻结父 stage obs id）；**不创建/不关闭 child AGENT**
2. `SubagentStart(child_agent_id, ...)` → join invocation → 生成并发送 child AGENT start observation → 注册 `child_agent_id → observation_id`
3. child `StageStarted` → 按事件 agent_id 查 registry，parent 固定为 child AGENT obs id；**不再调用 mark stack top**
4. child `LlmCallStart/End` → 继续 `(agent_id, step)` 管理 generation；parent 取该 child 的 active stage，**不得回退主 agent**
5. child 普通 `ToolStart/End` → 按事件 agent_id 查其 own ToolBatch / active stage；不取"栈顶 ToolBatch"
6. 子 agent 再发起 Agent tool → 以该 child agent_id 为父建立新 invocation（树状关系），child2 的 AGENT parent 冻结为 child1 当时的 active stage
7. 父 `ToolEnded(Agent tool)` → 只结束/记录父 Agent tool 本身的 output 并更新 invocation；**绝不关闭 child AGENT、绝不注销映射**
8. `SubagentStop` → 先保证 child 排队内容事件处理完 + ToolBatch flush，再以 Stop 实际时间关闭 child AGENT obs → execution 标记 completed → 回收 invocation（须已收到 Stop 且已处理对应 ToolEnded）

### 跨 forwarder 乱序：注册闸门（registration gate）

- child 内容事件先于 `SubagentStart` 到达：按 child_agent_id **有界缓存**，不得 fallback 挂主 agent
- `SubagentStart` 先于父 `ToolStart` 到达：暂存 Start，等待 invocation join
- join 完成后：先发送/登记 child AGENT start，再按原顺序重放缓存事件
- 超时 / 容量溢出 / 缺失关联：进入明确的 incomplete / diagnostic 分支（带 child_agent_id 与诊断属性），**不允许静默挂主 agent-run**

### 边界与防护

- **parent 冻结**：`SubagentStart` join 时冻结，后续不因活跃 stage 改变而重算（维持现有"AGENT parent 是发起时 stage"结构，避免嵌套/异步期间 parent 漂移）
- **循环引用防护**：child obs id ≠ parent obs id；parent 只能是发起 invocation 捕获的既有 stage 或明确主 agent fallback；拒绝 parent 位于 child 已知后代的情况
- **deferred_output 附着在 invocation**（而非栈顶 context）：Stop/ToolEnded 任意先后到达都不丢不重
- **基数放宽**：允许一个 invocation 持有多个 child id（待验证生产一对一语义后再收紧）
- **事件丢失降级**：若 `SubagentStart`/`ToolStart` 丢失，缓存等待或挂明确标记的 orphan/incomplete observation；不挂"当前 stage"/"栈顶"
- **`on_turn_end` 仅作异常兜底**，不作为正常 child 生命周期结束信号；bg subagent 跨 turn 的 tracer 生命周期需实现时确认（见"遗留待确认"）

### 明确不做（advisor 不建议路径）

继续修补 `mark_top_started()`、加延迟/yield_now/sleep、等"首个 StageStarted 再 ToolEnded"的时序猜测；仅让 ToolEnded 校验 tool_call_id 后 pop（修了错误 pop 但仍有短 AGENT obs）；未知 child 事件无条件 fallback 主 agent；以内部随机 UUID 替代事件侧 AgentId 作关联键；Stop 到达即删 invocation 数据。

### 遗留待确认（实施前）

1. subagent session 的 `agent_id` 与 `child_thread_id`（`SubagentStarted.instance_id`）是否同一值 —— 决定映射键直接可用或需一跳转换
2. bg subagent 跨 turn 时 tracer 生命周期（Stop 关闭路径的可行性）
3. Langfuse client 的 create/update 分离发送与 bridge batch/flush 语义（parent-first 保证）
4. `SubagentStart/Stop` 现有测试语义（`subagent_event_forwarder_test.rs:190/271/512`）与新增契约的一致性

## 验证方式（重构验收，advisor 定稿）

1. **状态机单元测试**（tracer 层）：`ToolStart→Start→child events→ToolEnded→Stop`（AGENT 覆盖完整窗口）；`ToolStart→ToolEnded→Start→child events→Stop`（不得提前关闭、无 17ms 空壳）；`Start→child events→Stop→ToolEnded`（deferred output 找回、不重复 flush）；乱序缓存重放（parent-first）；未知 agent_id / 缺失 Start / 重复 Start/Stop / 缓存溢出 → 明确 incomplete，不挂主 agent；parent 冻结 / parent≠child / 嵌套无环
2. **归属与并发单元测试**：两并行 subagent 事件交错（A.StageStart, B.StageStart, A.Llm, B.Tool, A.StageEnd, B.StageEnd）各自归属正确；顺序 subagent 人为重现本 trace 的 ToolEnded 提前消费顺序 → 无空壳、无主 agent 污染；嵌套 main→child1→child2 的 ToolBatch 互不串扰；同 step 不同 agent_id 的 generation 隔离
3. **bridge 集成测试**：两个独立、可控顺序的 event producer 模拟主/subagent forwarder + 共享 tracer lock，覆盖所有跨 task 乱序排列（不依赖 yield_now）；断言观测图中：child stage 的 parent 链为该 child AGENT、child LLM/tool 绝不指向主 agent-run、child AGENT `start ≤ 最早 child 事件` 且 `end ≥ 最晚 child 事件`、每个 obs 至多一个 parent、图无环、主 agent 既有结构不变
4. **端到端回归**：顺序双 subagent / 并行双 subagent / 嵌套 / fork / bg / 失败-cancel 各一例；判别指标：child AGENT 子节点数 > 0、活跃期 stage 不再 100% 指向主 agent-run、无"17-19ms 空壳 + 首个 stage 在其后"的时间关系、无 child 残留事件回退主 agent-run；回归 `cargo test -p peri-acp`、`cargo clippy --workspace --all-targets -- -D warnings`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 三路并行调查确认根因（调查报告：`.peri/plans/langfuse-subagent-event-path-report.md`、`.peri/plans/subagent-stack-audit.md`） |
| 2026-08-05 | Open | Open | agent | advisor（Opus）咨询定稿重构方案：A+B+C 身份注册表替代 LIFO 栈；controller decision 全部 adopted；补齐证据：SubagentStart/Stop 字段已完备仅缺 emit（`events_v2.rs:342-357`）、生产 emit 点已存在（`execute_fork.rs:110`、`execute_bg.rs:153`、`tool_dispatch.rs:304-308`）、v1 桥已定义（`events_v2_mapper.rs:209`） |
| 2026-08-11 | Open | Fixed | agent | 归档：身份注册表归属机制落地（8cadefbe fix(langfuse): 修复 subagent AGENT observation 不完整），修复记录见正文 |
