# bg agent / bg shell 调用后无响应

**状态**：Open
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-25

## 问题描述

在 TUI 中调用 bg agent（Agent 工具 `run_in_background: true`）或 bg shell（Bash 工具 `run_in_background: true`）后，任务似乎被启动但**完成后没有任何通知/结果注入到对话中**。主 agent 不会收到 bg 完成消息，TUI 的 bg task area 可能也没有正确显示完成状态。

## 症状详情

| 操作 | 期望 | 实际 |
|------|------|------|
| Agent(run_in_background: true) | 任务完成后，主 agent 收到系统提醒 `<system-reminder>` | 无响应，无通知 |
| Bash(run_in_background: true) | 命令完成后，结果出现在对话中 | 无响应，无通知 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 TUI 中发起一个需要 bg agent 或 bg shell 的请求
  2. 工具调用被 LLM 生成，run_in_background 参数为 true
  3. 工具立即返回（有 task_id），但后续无任何完成通知
- **环境**：macOS / 最近一天内的代码

## 涉及文件

### 事件链路（bg 结果注入路径）
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— bg SubAgent spawn + 完成时通过 `bg_event_sender` 发 `BackgroundTaskCompleted` + `on_bg_complete` 推 Defer
- `peri-middlewares/src/subagent/tool/define.rs` —— SubAgentTool 的 `bg_event_sender`/`on_bg_complete`/`background_registry` 注入点
- `peri-middlewares/src/middleware/terminal.rs` —— BashTool 的 bg shell 路径（`run_in_background` 分支）
- `peri-middlewares/src/subagent/spawner.rs` —— `spawn_background_fork()`（/bg 命令 + fork 路径共用）
- `peri-acp/src/session/async_router.rs` —— `route_bg_result()` 将 bg 结果推入 Session inbox
- `peri-acp/src/session/executor_helpers.rs` —— bg_event_pump（后台事件泵）
- `peri-acp/src/agent/builder.rs` —— `build_stage_context` 装配 SubAgentTool 的 bg 依赖

### 可疑变更（compact v2 重写 + 修复）
- `peri-agent/src/agent/compact_v2/projection.rs` —— **render_llm_view()** 纯函数 + has_changes() 判定（刚修复）
- `peri-agent/src/agent/compact_v2/planner.rs` —— plan_micro() 规划 truncated 目标 + reclaim_target（刚修复）
- `peri-agent/src/agent/compact_v2/micro.rs` —— micro_compact() 应用 truncated 标记
- `peri-agent/src/agent/stages/compact.rs` —— Compact 阶段入口（决定是否调用 compact_v2）
- `peri-agent/src/agent/stages/reason.rs` —— Reason 阶段（根据 has_changes() 决定用 render_llm_view 还是 visible）

### 已知相关历史 issue
- `spec/archive-issues/subagent/2026-07-11-hung-bg-agent-await-wake-block-forever.md`
- `spec/archive-issues/subagent/2026-07-09-bg-agent-loading-never-stops-after-first-turn.md`
- `spec/archive-issues/subagent/2026-07-07-bg-agent-complete-no-resume.md`
- `spec/issues/2026-07-25-has-changes-gate-blocks-compact-projection.md`（刚修复）

## 初步分析

### 时间线吻合
- `92fe66fb`（compact-v2 全面重写）→ `c0b21475`, `c7e77a3d`, `7a93601e`（compact 修复）→ `2817979c`（is_direct）→ `4a3b7994`（has_changes 修复）
- 用户反馈问题出现在最近一天内，与 compact v2 重写 + 后续修复高度吻合

### 关键转折点
`has_changes()` 修复前（commit `4a3b7994` 之前）：`estimated_tokens_saved > 0` 对短消息恒为 false → compact 从未真正生效 → LLM 看到完整原文。

**修复后**：`!self.actions.is_empty()` → compact **首次真正激活** → `render_llm_view()` 被调用 → 可能存在投影逻辑错误。

### 可能根因（待验证）

1. **compact 误伤了 bg 通知消息**：`plan_micro()` 可能将 bg 完成通知（Human 消息）纳入 truncated 范围，导致 LLM 看不到 bg 结果
2. **render_llm_view() 存在边界 bug**：对某些消息类型的投影不正确，可能导致消息内容丢失或格式错误
3. **is_direct() 变更的副作用**：虽然 Agent 和 Bash 工具正确声明了 `is_direct() → true`，但 bg agent/shell 内部使用的工具可能受过滤影响

### 已验证不相关
- ✅ 编译通过（`cargo build --workspace`）
- ✅ 单元测试通过（background tests 11 passed, bg tests 13 passed）
- ✅ Agent 和 Bash 工具均正确声明 `is_direct() → true`
- ✅ bg_event_pump 逻辑未在最近 commit 中变更

## 对抗验证结果（2026-07-25）

派出 3 个 bg agent 从不同角度独立验证，**实证了问题存在**：

| Agent | 任务 | 工具调用 | 耗时 | 结果 |
|-------|------|----------|------|------|
| #1 | compact v2 投影验证 | **0** | 1.7s | ❌ 未执行——启动后立即退出 |
| #2 | bg 事件链路追踪 | 未知 | 未知 | ⚠️ 未收到结果通知 |
| #3 | git 历史回归分析 | 少量 | 2.6s | ❌ 仅输出部分 git 命令，无分析结论 |

### 深度代码分析

**排除 compact v2**：经过对 `projection.rs`、`planner.rs`、`micro.rs` 的完整审查：
- `plan_micro()` 仅对 AI 消息中的 ToolExchange 生成 action，不处理 Human 消息
- `render_llm_view()` 正确保留 Human/System 消息（无 block_action 时原样返回）
- `visible_messages()` 仅按 `excluded` flag（非 `truncated`）过滤
- **结论：compact v2 不会直接导致 bg 通知丢失**

**排除事件链路断裂**：经过对 builder.rs → executor.rs → execute_bg.rs 的完整追踪：
- `on_bg_complete` 正确从 `async_router.route_bg_result` 注入
- `push_defer` → `drain_for_end` 路径完整
- bg_event_pump 生命周期正确
- **结论：bg 事件链路完整**

**新假设**：bg 任务可以 spawn 并完成（`system-reminder` 正常到达），但 **bg subagent 的 LLM 交互阶段产出异常**——Agent #1 的 0 tool calls 说明 LLM 要么无法生成 tool_use，要么系统 prompt/tools 传递有误。

### 下一步调查方向

1. **bg agent 的系统 prompt 构造**：`spawn_background_fork()` 中的 `build_bg_fork_directive()` 或 `build_fork_directive()` 是否正常
2. **bg agent 的 tools 传递**：`build_subagent_middlewares()` → `build_agent_from_def` 是否受 `is_direct()` 变更间接影响
3. **bg agent 的 Reason 阶段**：subagent 的 `run_react_loop` 首轮 LLM 调用是否正确拿到 tools 和 messages

### Agent 2 对抗验证详细发现

| 断言 | 验证结果 |
|------|---------|
| BashTool bg_registry=None | ❌ **误报** — `TerminalMiddleware::collect_tools()` 正确通过 `self.bg_registry.clone()` 传递 |
| SubAgentTool on_bg_complete=None | ❌ **误报** — `builder.rs:411-412` 正确注入 via `with_on_bg_complete()` |
| BashTool run_in_background 无事件推送 | ❌ **误报** — 代码中 `register_with_kind()` + `complete()` 调用存在（`terminal.rs:271-276`） |
| `/bg` 命令路径 BashTool 无 registry | ✅ **确认** — `bg.rs:80` 使用 `TerminalMiddleware::build_tools()` 而非 `collect_tools()`，创建的 BashTool 无 registry |

### 最终结论

经过 3 重验证（代码分析 + 对抗验证 + 实证测试），根因链如下：

1. **bg subagent 的 Reason 阶段异常**：subagent 的 LLM 调用返回了文字但**无 tool calls**（Agent #1: 0 tool calls 实证）

2. **可能的触发机制**：`is_direct()` 变更（commit `2817979c`）在 `reason.rs:113-117` 过滤工具。虽然 Agent/Bash 主工具标记为 direct，但 **subagent 在 `build_agent_from_def` → `build_subagent_middlewares` 过程中获得的工具集可能包含未标记 `is_direct()` 的工具**。当 subagent 的 Reason 阶段同样执行 `.filter(|t| t.is_direct())` 时，如果某个关键中间件工具缺失，subagent 的行为就会异常。

3. **bg shell 的额外风险**：`/bg` 命令路径（`bg.rs`）使用 `TerminalMiddleware::build_tools()` 而非 `collect_tools()`，导致 BashTool 创建时无 registry——这意味着通过 `/bg` 命令启动的 bg shell 任务无法使用 `run_in_background` 功能。

### 建议修复方向

1. **审查 subagent 的完整 tools 列表**：打印 subagent 的 `Reason` 阶段 `tool_refs` 中每个工具名，确认 `is_direct()` 标记是否完整
2. **修复 `/bg` 路径**：`bg.rs` 中改用 `build_tools_with_registry()` 或使用 `collect_tools()`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建 |
| 2026-07-25 | Open | Open | agent | 更新：对抗验证结果——排除 compact v2 / 事件链路，确认 bg subagent LLM 交互阶段异常 |
