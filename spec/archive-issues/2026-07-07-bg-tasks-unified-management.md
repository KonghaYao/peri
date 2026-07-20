# 后台任务统一管理（bg agent / workflow / bg shell）


> 归档于 2026-07-20，原路径 spec/issues/2026-07-07-bg-tasks-unified-management.md
**状态**：Fixed
**Triage**：ready-for-agent
**优先级**：高
**创建日期**：2026-07-07

---

## Problem Statement

用户在 TUI 中使用后台任务时遇到三类痛点：

1. **后台 agent 有问题**：用户通过 `/bg` 命令或 Agent 工具启动的后台 agent，在下一轮对话中"消失"——LLM 看不到上一轮启动的后台 agent，无法查询结果、无法取消、状态栏也无任何指示。根因是 `BackgroundTaskRegistry` 在每个 `execute_prompt` 调用中重新创建（per-prompt 生命周期），跨 prompt 后失去对原 tokio task 的追踪。

2. **Workflow 不在主线程显示有问题**：LLM 调用 Workflow 工具后，工具立即返回 `run_id`（fire-and-forget），主 loading spinner 立即消失，输入框恢复可输入状态。但 workflow 子进程可能在跑几十秒到几分钟，用户完全不知道它还在跑，容易以为已经完成或卡住。同时 workflow 的 `WorkflowProgress` 事件在路由层被丢弃，进度完全不可见。

3. **后台 shell 不存在**：用户无法让单条 shell 命令在后台运行（不阻塞主 agent）。当前唯一路径是启动一个完整的后台 SubAgent（带 LLM 调用），开销大、语义重。

4. **状态栏无任何任务指示**：状态栏只显示权限/cwd/model/CPU/MEM，用户无法一眼看出当前有多少后台任务在运行。

## Solution

将三类"有界、用户主动启动、有完成时刻"的任务——**bg agent（后台子 agent）+ workflow（工作流）+ bg shell（后台 shell）**——统一到同一个 `BackgroundTaskRegistry` 中管理，在状态栏显示运行中计数，在 Tasks 面板提供详情查看和取消操作。

核心改动是把 `BackgroundTaskRegistry` 从 per-prompt 生命周期**提升到 `SessionState` 级**，跟现有 `WorkflowMiddleware` 同构（session 级、跨 prompt 存活），从根本上修复 bg agent 跨 prompt 丢失的 bug。

为 `BashTool` 增加 `run_in_background: bool` 参数提供轻量后台 shell 能力（不依赖完整 SubAgent）。Workflow 启动时同步注册到 registry，跟 bg agent / bg shell 完全同构。

thread 切换前若有运行中的后台任务，弹 ConfirmPopup 二次确认，避免用户无意中切换走错过完成通知。

## User Stories

### 状态栏可见性

1. 作为 TUI 用户，我想在状态栏看到当前后台任务的运行中数量（按 shell/agent/workflow 分类），这样我能一眼知道有没有任务在跑。
2. 作为 TUI 用户，我想在没有任何后台任务时看到状态栏干干净净（无任何任务指示），这样默认状态不嘈杂。
3. 作为 TUI 用户，我想看到状态栏显示 ` · 2 shell 1 agent 1 workflow` 这样的紧凑格式，这样我能区分任务类型。
4. 作为 TUI 用户，我想在只有某一类任务时只看到那一类（如 ` · 1 workflow`，零分类省略），这样信息密度高。
5. 作为 TUI 用户，我想状态栏任务计数跟 Row1 现有 CPU/MEM 字段视觉一致（同 ` · ` 分隔），这样学习成本为零。
6. 作为 TUI 用户，我想状态栏不显示已结束任务（completed/failed/cancelled），这样状态栏只反映"当前在跑"。
7. 作为 TUI 用户，我想状态栏数字不带 spinner 动画，这样跟其他字段风格统一。

### 跨 prompt 持久性（核心 bug 修复）

8. 作为 TUI 用户，我在第 1 轮启动了 bg agent 后，想在第 2、3、N 轮仍然能在状态栏和 Tasks 面板看到它仍在运行。
9. 作为 TUI 用户，我想在 bg agent 完成时收到通知（无论当前在第几轮），这样我能看到它的输出。
10. 作为 TUI 用户，我想在 Tasks 面板选中一个早期启动的 bg agent 并取消它，即使已经过去了多轮对话。
11. 作为 LLM 调用者，我想在主对话中通过 AgentResult 工具查询早期启动的 bg agent 的状态/输出，即使已经过去了多轮对话。

### Workflow 可见性

12. 作为 TUI 用户，我在 LLM 调用 Workflow 工具后，想在状态栏看到 ` · 1 workflow` 指示，这样我不会以为 workflow 已经完成。
13. 作为 TUI 用户，我想在 workflow 完成时收到顶部通知条反馈，这样我能感知结束。
14. 作为 TUI 用户，我想在 Tasks 面板看到 workflow 任务的运行时间和摘要，这样我能判断是否需要等待。
15. 作为 TUI 用户，我想在 workflow 跑偏时通过 Tasks 面板取消它，这样不必等它自然结束。

### 后台 shell（新增能力）

16. 作为 LLM 调用者，我想调用 Bash 工具时传 `run_in_background: true` 让长跑命令在后台执行，这样主对话不被阻塞。
17. 作为 LLM 调用者，我想在 Bash bg 启动后立即拿到 `task_id`（"已在后台启动"），这样我能继续干别的。
18. 作为 LLM 调用者，我想在 bg shell 完成时收到通知（输出 + exit code），这样我能根据结果决定下一步。
19. 作为 LLM 调用者，我想在 bg shell 失败（exit != 0）时也收到通知，这样我能根据错误决定是否重试。
20. 作为 LLM 调用者，我想在并发上限达到时收到 tool_error（"已达 shell 并发上限 5/5"），这样我能决定是等待还是取消现有任务。
21. 作为 TUI 用户，我想通过 Bash 工具后台跑 `npm test`、`npm run dev`、`gradle build` 等长跑命令而不阻塞对话。
22. 作为 TUI 用户，我想在 Tasks 面板看到后台 shell 的 task_id、运行时间、输出预览（前 500 字符）。
23. 作为 TUI 用户，我想取消一个跑偏的后台 shell（如发了死循环命令），这样系统资源不被持续占用。

### Tasks 面板交互

24. 作为 TUI 用户，我想在 Tasks 面板上方看到 Background Tasks section（在现有 Cron Jobs 上方），这样能区分"运行中任务"vs"调度配置"。
25. 作为 TUI 用户，我想在 Tasks 面板用 `Up/Down` 切换任务选中项，`d + Enter` 取消选中的任务，这样不必记新快捷键。
26. 作为 TUI 用户，我想取消操作的快捷键跟现有 Cron 列表一致（`d` 删除），这样交互一致。
27. 作为 TUI 用户，我想在 Tasks 面板看到每个任务的 kind（shell/agent/workflow）、task_id、摘要、运行时间。
28. 作为 TUI 用户，我想在 Tasks 面板看到已完成任务的最终状态（success/failed）和输出预览，这样不必查日志。
29. 作为 TUI 用户，我想在 Tasks 面板的 Background Tasks section 看到 section 标题带计数（如 `Background Tasks (3)`），这样能快速感知规模。

### Thread 切换保护

30. 作为 TUI 用户，我想在切换 thread 时如果有运行中的后台任务，弹 ConfirmPopup 列出任务清单（`2 shell 1 agent 0 workflow`），这样我不会无意中切走错过完成通知。
31. 作为 TUI 用户，我想在 ConfirmPopup 中按 `Enter` 确认切换、`Esc` 取消切换，这样快捷键跟其他弹窗一致。
32. 作为 TUI 用户，我想在所有后台任务都完成后切换不再被拦截（任务归零自动放行），这样不会被打扰。
33. 作为 TUI 用户，我想在 ConfirmPopup 中看到任务清单的具体分类计数，这样我知道切走的是什么。

### 完成通知

34. 作为 TUI 用户，我想在任意后台任务完成时看到顶部通知条（1.5s 后消失），如 `[✓] shell bg-abc 完成 (12s)`，这样有即时反馈。
35. 作为 TUI 用户，我想通知条只显示完成（不显示失败/取消细节），细节去 Tasks 面板看，这样通知条不嘈杂。
36. 作为 LLM 调用者，我想在后台任务完成时（无论成功失败）主对话下一轮自动收到 `[后台任务 XXX 已完成] 输出：...` 这样的合成消息，这样我能基于结果继续推理。

### 跨 thread 隔离

37. 作为 TUI 用户，我想状态栏只显示**当前 thread** 的后台任务计数，不显示全局聚合，这样我专注于当前会话上下文。
38. 作为 TUI 用户，我想每个 thread 的后台任务互相独立（thread A 的 bg agent 不会污染 thread B 的状态栏），这样多任务并行不混乱。

## Implementation Decisions

### 核心架构：Registry 生命周期提升

- `BackgroundTaskRegistry` 从 per-prompt 生命周期（在 `execute_prompt` / `build_agent_components` 内部 new）**提升到 `SessionState` 级**（一个字段），跟现有 `WorkflowMiddleware` 同构（session 级、跨 prompt 存活）。这是修复 bg agent 跨 prompt 丢失 bug 的根本改动。
- 所有创建点（executor 的 `bg_registry_for_cmd`、builder 的 `background_registry`）改为从 `SessionState` 取，不再本地 new。
- registry 严格 thread-scoped：每个 SessionState 持有自己的 registry，thread 切换天然隔离，无需 thread_id 路由。

### 统一三类任务的 registry 抽象

- `BackgroundTask` 增加 `kind: BgTaskKind` 字段，枚举 `Shell / Agent / Workflow`。
- `BackgroundTask` 增加 `cancel_handle: BgCancelHandle` 字段，enum 持有不同 kind 的取消句柄：
  ```rust
  enum BgCancelHandle {
      Abort(AbortHandle),         // bg agent: tokio task abort
      Kill(KillTx),               // workflow: kill channel + child process kill
      Pid(u32),                   // bg shell: OS process kill
  }
  ```
  cancel 时 match 分发，对外 API 统一为 `registry.cancel(task_id)`。
- WorkflowTool::invoke() 启动 workflow 时**同步调 registry.register()** 注册一项（kind=Workflow），workflow 完成/失败/取消时调 complete()/cancel() 注销。workflow 的执行逻辑不变（仍走 WorkflowMiddleware），只在边界多一次 register/complete 调用。

### 新增 bg shell 能力

- `BashTool` struct 增加 `bg_registry: Option<Arc<BackgroundTaskRegistry>>` 字段，由 `TerminalMiddleware` 构造时传入（registry 提升到 SessionState 后，所有中间件都能拿到 Arc）。
- `BashTool` 的 `parameters` JSON schema 增加 `run_in_background: bool`（默认 false）。
- `invoke` 中：
  - `run_in_background == false`：走现有同步执行路径（兼容性不变）。
  - `run_in_background == true`：
    - 检查 registry 当前 Shell 计数 < 5，否则返回 tool_error `"已达 shell 并发上限 (5/5)，请等待现有任务完成或取消其中一个"`。
    - `tokio::spawn` 跑 `process::shell_command`，捕获 OS PID 存到 cancel_handle。
    - 注册到 registry，立即返回 tool_result `"已在后台启动 shell（task_id: bg-xxx, pid: 12345）。完成时会通知你。"`。
    - 任务完成时（含 exit != 0），输出（stdout + stderr 合并）截断 500 字符作为 `output_preview`，调 registry.complete()。
    - 启动失败（spawn 失败、命令不存在）立即返回 tool_error，不注册到 registry。

### 并发上限（写死常量，不配置）

- `Shell: 5` / `Agent: 3` / `Workflow: 3`，每 kind 独立计数，无总上限（三种资源占用特征不同，混总上限会互相挤占）。
- 超出立即返回 tool_error（不排队、不静默丢弃），LLM 能理解并决策。
- 上限值写死为 const，不出现在 settings.json 配置面板。

### ACP 事件协议扩展（4 个新事件）

新增 4 个 ACP 事件，全部走现有 `peri/unstable-event` 单向推送通道：

| 事件 | 触发时机 | payload |
|------|---------|---------|
| `BgTaskSnapshot` | session/load 完成（ViewCommit 之后） | `{ tasks: Vec<BgTaskInfo> }`（新 thread 当前所有运行中任务） |
| `BgTaskStarted` | 任务启动 | `{ task_id, kind, summary, started_at }` |
| `BgTaskCompleted` | 任务结束（含失败 success=false） | `{ task_id, success, output_preview, duration }` |
| `BgTaskCancelled` | 用户主动取消 | `{ task_id, reason }` |

`BgTaskInfo.kind` 枚举 `Shell / Agent / Workflow`。

事件天然携带 session 上下文（ACP 协议基于 session_id 路由），无需额外 thread_id 字段。

### Workflow 双路径通知清理

- workflow 完成**Path B**（注入主 agent inbox 续跑）保留不变。
- workflow 完成**Path A**（旧的通知条 ExecutorEvent）废弃。
- 新的 `BgTaskCompleted` ACP 事件**同时驱动**：Tasks 面板更新（从 BG_TASKS 移除）+ 顶部通知条显示。

### TUI 数据流

- 新增 atom `BG_TASKS: Vec<BgTaskInfo>`（当前 thread 的任务列表，session/load 时由 BgTaskSnapshot 整体替换，后续 Started/Completed/Cancelled 增量更新）。
- thread 切换走现有 `BRIDGE_RESET_COUNTER` 机制，acp_bridge 检测到 counter 变更时清空 BG_TASKS（跟 VIEW_MODELS 同构）。
- session/load 完成后 agent 推 `BgTaskSnapshot`，TUI 整体替换 BG_TASKS。
- 不持久化跨 thread map（不维护 `HashMap<ThreadId, Vec<BgTaskInfo>>`），切换走 Snapshot 重置。

### 状态栏渲染

- Row1 末端追加段：` · {n} shell {n} agent {n} workflow`（按这个顺序）。
- 零分类省略（如 `3 shell 0 agent 1 workflow` → `3 shell 1 workflow`）。
- 全为零时整段连同前导 ` · ` 一起隐藏，状态栏不出现任何痕迹。
- 数字 1 时仍带数字（`1 agent` 而非 `agent`），格式简单一致。
- 不补 spinner，纯静态文字，跟 CPU/MEM 风格一致。

### Tasks 面板扩展

- 现有 Tasks 面板（`tasks.rs`）加 Background Tasks section，渲染在 Cron Jobs section 上方。
- 布局：
  ```
  ┌─ Tasks ─────────────────────────┐
  │ ▼ Background Tasks (3)          │
  │   ⏵ shell  bg-abc1  npm test    │
  │     (running 12s, pid 12345)    │
  │   ⏵ agent  bg-def2  explorer    │
  │     (running 5s)                │
  │   ⏵ workflow bg-ghi3  plan.md   │
  │     (running 30s)               │
  │ ▼ Cron Jobs (2)                 │
  │   */5 * * * *  build check      │
  │   0 * * * *    sync repo        │
  └─────────────────────────────────┘
  ```
- `Up/Down` 切换选中项（跨 section），`d + Enter` 取消选中任务（调 registry.cancel()），快捷键跟现有 Cron 列表删除一致。
- 已完成任务从 atom 移除（不保留历史），状态栏不计入。面板只展示运行中任务。

### thread 切换拦截

- thread 切换走现有 `THREAD_LOAD_TX` → `thread_load_consumer` → `BRIDGE_RESET_COUNTER +1` → `session/load` 路径，不变。
- 切换前检查 `BG_TASKS` 是否非空：非空时设置 `POPUP_KIND = Confirm`，写入 `CONFIRM_PAYLOAD`（任务清单 + 文案）。
- ConfirmPopup 复用现有 PopupOverlay 基础设施：
  ```
  ┌─ 切换 thread 确认 ────────────────────────┐
  │ 当前 thread 有 3 个后台任务仍在运行：     │
  │   ⏵ 2 shell  1 agent  0 workflow          │
  │                                           │
  │ 切换后这些任务继续在后台执行，但当前视图  │
  │ 不再显示其状态。                          │
  │                                           │
  │ Enter 切换    Esc 取消                    │
  └───────────────────────────────────────────┘
  ```
- `Enter` 触发实际 `THREAD_LOAD_TX.send(target_id)`；`Esc` 关闭弹窗。
- 任务归零时（`BG_TASKS.is_empty()`）自动放行，不弹窗。
- 拦截阈值：哪怕只有 1 个 bg 任务也拦截（零意外）。

### 完成语义（三类任务统一）

- 任意 kind 完成时（含失败），通过现有 `MessageQueue` + `bg_results` 通道注入主 agent inbox（Defer 消息），唤醒续跑。
- 主 agent 下一轮看到合成消息：`[后台任务 bg-xxx 已完成] 输出：...` 或 `[后台任务 bg-xxx 失败] exit code: 1, stderr: ...`。
- 用户主动取消不触发续跑（只发 BgTaskCancelled 事件，TUI 通知条不显示取消，仅面板状态变化）。
- 任务完成同时显示顶部通知条（1.5s 消失）：`[✓] shell bg-abc 完成 (12s)` 或 `[✗] shell bg-abc 失败 (8s)`。

### task_id 格式

- 统一 `bg-{uuid}`（如 `bg-a1b2c3d4`），跟现有 bg agent 格式一致。
- 不暴露 OS PID 作为 task_id（pid 仅用于 cancel_handle 内部）。

### `/bg` 命令兼容

- `/bg <prompt>` 命令保留，仍启动 SubAgent（带 LLM 跑后台 agent），跟 Bash bg 参数并存。
- 两种入口语义不同：`/bg` = 后台 agent（带 LLM），Bash bg = 后台 shell（无 LLM）。

### 不做的 ADR 约束

- 不修 `WorkflowProgress` 路由 bug（`router.rs:226` 丢弃）—— 留作后续增量。
- 不做实时输出 —— 任务条目只显示 `output_preview`（500 字符），完整输出走日志文件或后续面板增量。
- 不做 `/bg-list` / `/bg-cancel <id>` 命令 —— 用户用 Tasks 面板就够了。
- 不做并发上限配置（settings.json）—— 写死 const。
- 不持久化跨 thread map —— thread 切换 Snapshot 重置即可。

## Testing Decisions

### 测试哲学

只测外部行为（"给这个输入，得到那个输出"），不测内部实现细节（不 mock 私有方法、不 assert 私有字段）。优先复用现有测试 seam，新增 seam 提到最高可能点。

### 主 Seam：ACP 事件 → atom 转换契约（最高层）

**位置**：`peri-tui/src/kit/acp_events.rs` 测试模块（跟现有 `drain_input_buffer` 测试同 seam）。

测的是 4 个新事件（`BgTaskSnapshot / Started / Completed / Cancelled`）的解码 → atom 更新函数。这是端到端数据流的契约边界：只要这个函数把事件正确映射到 `BG_TASKS` atom 的更新（替换/追加/移除/标记），整个 TUI 数据流就对了。

测试用例：
- Snapshot 事件整体替换 BG_TASKS
- Started 事件追加新任务
- Completed 事件移除（或保留标记 + 通知条触发）
- Cancelled 事件移除 + 不触发通知条
- 零分类省略、全零隐藏的状态栏派生函数

**为什么这是最高 seam**：ACP 协议是 agent 进程与 TUI 的唯一桥梁。这一层契约对了，上游 agent 怎么 spawn / 怎么注册都不影响 TUI 正确性；下游 atom 怎么驱动 ratatui-kit 组件也不影响契约正确性。

### 次 Seam 1：BackgroundTaskRegistry 行为

**位置**：`peri-middlewares/src/subagent/background_test.rs`（已存在）。

测的是 registry 公开 API 的行为：register/complete/cancel/list_tasks/count_by_kind。

测试用例：
- 注册 3 类任务后 count_by_kind 返回正确分类计数
- cancel 调用对应 kind 的 cancel_handle 分发（用 mock handle 验证 abort/kill/pid 三种路径）
- 并发上限拒绝：第 6 个 Shell 注册返回 Err
- complete 触发 notification_tx 推送

**为什么不更高**：这是契约以下的单元行为，验证 enum 分发逻辑。但不测内部 HashMap 状态，只测公开 API 返回值。

### 次 Seam 2：BashTool bg 参数

**位置**：`peri-middlewares/src/middleware/terminal_test.rs`（跟现有 `format_command_output` 测试同 seam）。

测的是 BashTool::invoke 在不同 input 下的返回值。

测试用例：
- `run_in_background=true` + sleep 1s 命令 → 立即返回（< 100ms）含 task_id 的字符串，registry 计数 +1
- `run_in_background=false` → 走原同步路径，行为跟未加参数一致（兼容性）
- `run_in_background=true` + 不存在的命令 → 立即返回 tool_error，registry 计数不变
- 已有 5 个 Shell 时第 6 个 → 返回 tool_error `"已达 shell 并发上限"`

**为什么这个 seam**：BashTool 是 LLM 调用的契约边界，invoke 是其入口。测 invoke 就覆盖了 bg shell 的所有外部行为。

### 三级 Seam：Tasks 面板交互 + thread 切换拦截

**位置**：`peri-tui/src/kit/panels/tasks.rs` 测试模块 + `thread_load_consumer.rs` 测试模块（已存在）。

测的是面板渲染（给定 BG_TASKS 状态生成什么 Line）+ 切换决策（给定 BG_TASKS 是否弹 ConfirmPopup）。

测试用例：
- BG_TASKS 含 3 项时 Background Tasks section 渲染 3 行 + section 标题带 `(3)`
- `d + Enter` 在选中第 0 项时调用 registry.cancel(task_id)
- BG_TASKS 非空时 THREAD_LOAD_TX.send 不立即触发，先设置 POPUP_KIND = Confirm
- BG_TASKS 为空时 THREAD_LOAD_TX.send 立即触发，不弹窗

**为什么这个 seam**：面板渲染是 pure function（state → Line），切换决策也是 pure function（state → action）。可单测验证。

### 不测的内容

- **不测 workflow 子进程行为**：fire-and-forget，由 peri-workflow crate 自己保证。
- **不测 Bash 实际命令执行**：现有 `format_command_output` 测试已覆盖。
- **不测 cron 调度**：本次不动 cron。
- **不测 OS 进程 kill 的实际效果**：cancel_handle 持有 pid 后 kill 是否真杀进程，由 OS 保证；只测 cancel() 函数被调用时是否触发对应 kill 路径（mock 验证）。

### 测试风格遵循项目惯例

- 命名 `test_<对象>_<场景>`，中文注释/断言
- Arrange-Act-Assert 无空行
- Mock 用 `make_` 前缀，不用 mockall 生成 Mock struct
- `#[serial]` 用于依赖全局 atom 的并发敏感测试
- 测试分离 `_test.rs`（≥30 行）

## Out of Scope

### 显式不做的功能

- **`WorkflowProgress` 路由 bug 修复**（`router.rs:226` 丢弃 + TUI 不处理）：留作后续增量。本次只做生命周期 4 事件（Snapshot/Started/Completed/Cancelled），workflow 子 agent 的实时进度不可见。
- **实时输出**：bg shell/bg agent 的实时 stdout 不流入 TUI，只在完成时一次性提供 500 字符预览。
- **完整输出查询**：不通过面板查完整输出，需要时走 `~/.peri/logs/` 或后续独立 PRD。
- **`/bg-list` / `/bg-cancel <id>` 命令**：用户用 Tasks 面板 + 快捷键 d 就够了。
- **并发上限可配置**：写死 Shell 5 / Agent 3 / Workflow 3，不进 settings.json。
- **跨 thread 任务持久化**：不维护 `HashMap<ThreadId, Vec<BgTaskInfo>>`，thread 切换靠 Snapshot 重置。
- **Workflow 面板（`workflow.rs`）改造**：保持现状（静态信息面板），不接入实时数据。
- **SubAgent 内部 bash 命令的 PID 暴露**：只暴露 bg shell 的 PID（用于 cancel），SubAgent 内部 bash 仍是黑盒。
- **失败任务的高优先级标记**：失败和成功一样走 BgTaskCompleted(success=false)，不单独事件、不特殊高亮。
- **Cron 触发产生的执行进 registry**：cron 是调度器，触发后的 turn 跟普通用户输入同构，不进 registry。

### 兼容性保证

- **`/bg <prompt>` 命令保留**：仍启动 SubAgent（带 LLM），不变。
- **现有 Bash 工具调用零影响**：`run_in_background` 默认 false，未传参时行为完全一致。
- **现有同步 SubAgent 行为不变**：不走 registry 注册路径。
- **现有 bg agent 启动后注入 inbox 续跑的语义不变**：只是 registry 提升生命周期，通知通道复用。

## Further Notes

### 关键 TRAP（来自代码库 CLAUDE.md，本 PRD 实施时必须遵守）

1. **registry 提升后的迁移**：`executor.rs:448` 和 `builder.rs:383` 都要改成从 SessionState 取，不能遗漏任一创建点，否则会出现"两个 registry 不同步"的诡异 bug。
2. **ACP 通知覆盖度**：所有 bg 任务生命周期事件（包括取消）必须完整经 acp_notifier 转发，遗漏导致 UI 状态残留（参考 issue_2026-07-06-enter-hello-cpu-spike）。
3. **render body 中禁止写 atom**：状态栏、面板渲染函数只读 atom，所有状态变更走事件处理器（参考 issue_2026-07-03-tui-double-slash-cpu-spike）。
4. **ratatui-kit hook 顺序**：Tasks 面板和 ConfirmPopup 内 `hooks.use_*` 调用必须在任何 `if`/`match`/`return` 之前（参考 issue_2026-07-05-enter-clear-hook-mismatch-panic）。
5. **u16 坐标计算用 saturating_add/sub**：状态栏追加段、面板布局中所有坐标运算（参考 issue_2026-07-05-message-area-crashes-and-rendering）。
6. **BRIDGE_RESET_COUNTER 必须 +1**：thread 切换前（即使有 ConfirmPopup 拦截）最终确认切换时要 +1，acp_bridge 才会清空 BG_TASKS（跟 VIEW_MODELS 同构）。

### 实施建议顺序

虽然 PRD 不规定实施顺序，但建议从核心 bug 修复（Registry 提升）开始，再做状态栏（用户最直接感知），最后做 bg shell 工具和面板扩展。每一步可独立验证。

### 后续可能的 PRD（本 PRD 范围外）

- **WorkflowProgress 接入**：修路由丢弃 bug，给 Tasks 面板 workflow 条目加进度子项。
- **实时输出**：bg shell 输出实时流入 Tasks 面板（类似 tail -f）。
- **完整输出查询**：Tasks 面板按 Enter 打开完整输出（分页 / 写文件）。
- **并发上限可配置**：settings.json 加 `background_limits` 字段。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建（grilling skill 访谈 + to-prd skill 生成） |
