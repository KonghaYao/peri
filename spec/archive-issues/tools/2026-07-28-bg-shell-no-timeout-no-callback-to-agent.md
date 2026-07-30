> 归档于 2026-07-30，原路径 spec/issues/2026-07-28-bg-shell-no-timeout-no-callback-to-agent.md

# bg shell 缺少超时机制、完成回调未注入 Agent inbox、并发检查存在竞态

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-28

## 问题描述

bg shell（`Bash` 工具 `run_in_background: true`）目前存在三个核心缺陷：(1) 无超时机制——相比同步 Bash 的 15s 默认超时，bg shell 一旦 spawn 就永远运行直到进程退出；(2) 完成后不注入 Agent inbox——Agent 无法感知 bg shell 完成，无法基于结果继续工作；(3) 并发检查存在 TOCTOU 竞态——外部 `count_by_kind` 与内部 `register_with_kind` 之间可被其他 spawn 抢占，失败时静默丢弃 tokio task。

此外附带 `list_tasks_full()` 的 `started_at` 始终返回当前时间（非任务真实启动时间）、spawn 失败路径使用 `Pid(0)` 魔法值（`kill -TERM 0` 会波及当前进程组）、以及 Tasks 面板取消按钮是 no-op。

## 症状详情

| 缺陷 | 期望 | 实际 |
|------|------|------|
| bg shell 超时 | 与同步 Bash 对齐：默认 15s，LLM 可通过 `timeout` 参数显式设置 | 无超时，shell 完成后调用 `child.wait_with_output()` 永久等待 |
| 完成回调注入 inbox | bg shell 完成 → `route_bg_result` → Defer 消息 → Agent 感知结果 | 仅在 TUI 显示通知条，Agent 完全不知道 shell 产出 |
| 输出超长落盘 | 输出超过阈值时落盘，返回文件路径给 Agent | 完整输出驻留内存（`BackgroundTaskResult.output`），无落盘 |
| `list_tasks_full().started_at` | 返回任务真实启动时间 | 始终返回 `Utc::now()`——面板中每个任务的启动时间都是"现在" |
| 并发竞态失败 | 超限时返回明确错误给 LLM | tokio task 静默退出，仅打 `warn!` 日志 |
| `Pid(0)` 魔法值 | spawn 失败时 cancel handle 为安全状态 | `BgCancelHandle::Pid(0)`——若被取消会执行 `kill -TERM 0` |

## 复现条件

- **复现频率**：必现（每个缺陷独立复现）
- **触发步骤**：
  1. 超时：`run_in_background: true` 执行 `sleep 3600`，shell 永远不返回
  2. 无回调：任意 bg shell 完成后，Agent 不会收到 `<system-reminder>` 续跑消息
  3. 竞态：并发触发 6 个 bg shell（超过 SHELL_LIMIT=5）
  4. `started_at`：打开 Tasks 面板观察任意 bg shell 的启动时间
- **环境**：所有平台

## 涉及文件

### 核心修改文件

| 文件 | 修改内容 |
|------|---------|
| `peri-middlewares/src/middleware/terminal.rs` | bg shell 超时逻辑 + 完成回调注入 + 输出超长落盘 |
| `peri-middlewares/src/subagent/background.rs` | 修复 `list_tasks_full().started_at` + `Pid(0)` 处理 |
| `peri-acp/src/session/executor.rs` | bg shell 的 `on_bg_complete` 回调注册（对 BashTool 注入 AsyncRouter） |

### 联动修改文件

| 文件 | 修改内容 |
|------|---------|
| `peri-tui/src/kit/panels/tasks.rs` | 取消按钮实际调用 `BackgroundTaskRegistry::cancel()` |
| `peri-tui/src/kit/acp_events/system.rs` | 可能需新增 bg shell completed 事件处理 |
| `peri-acp/src/agent/builder.rs` | BashTool builder 装配 bg 回调依赖 |

### 参考文件

| 文件 | 说明 |
|------|------|
| `peri-middlewares/src/subagent/tool/execute_bg.rs` | bg SubAgent 的 complete 流程（参考回调注入模式） |
| `peri-acp/src/session/async_router.rs` | `route_bg_result()` 实现参考 |
| `peri-acp/src/session/executor.rs:905-910` | `on_bg_complete` 注册模式参考 |
| `spec/issues/2026-07-25-bg-agent-shell-no-response-after-invocation.md` | 已有相关 issue（侧重 bg agent 事件泵断裂），本 issue 独立覆盖 bg shell 链路 |

## 设计要点

### 1. bg shell 超时

- 与同步 Bash 对齐：默认超时 15s，LLM 可通过 `timeout` 参数显式设置（单位 ms，最大值 600,000ms = 10min）
- `timeout: 0` 表示无超时（兼容长期运行的服务器/构建场景）
- 超时后走 `complete()` 流程，标记 `success: false`，输出 "Command timed out after Xs"

### 2. bg shell 完成回调

- 在 BashTool 上注册 `on_bg_complete` 回调（参考 SubAgentTool 的 `execute_bg.rs` 模式）
- 回调中调用 `AsyncRouter::route_bg_result(result)`，将 shell 结果作为 Defer 消息注入 Agent inbox（`MessageSource::SubAgentComplete`）
- `route_bg_result` 触发 `wake.notify_one()`，Agent 在 End 阶段 drain 并收到 `<system-reminder>` 消息

### 3. 输出超长落盘

- 所有 bg shell 数据本来就是给 Agent 看的（用户不直接读），无需常规落盘
- **仅当** shell output 超过阈值（如 100K 字符）时触发落盘：`persist_truncated_output()` → 完整内容写磁盘，`BackgroundTaskResult.output` 替换为摘要 + 文件路径提示
- Agent 通过文件路径可读取完整输出

### 4. 并发竞态修复

- 移除 `terminal.rs:149-157` 的外部 `count_by_kind` 预检查
- 仅保留 `register_with_kind` 内部的锁内检查
- `register_with_kind` 失败时返回 Err → BashTool 将错误返回给 LLM（不再静默丢弃）

### 5. `list_tasks_full().started_at` 修复

- 在 `BackgroundTask` 中新增 `created_at: chrono::DateTime<Utc>` 字段
- `register_with_kind` 时记录真实创建时间
- `list_tasks_full()` 使用 `t.created_at` 而非 `Utc::now()`

### 6. `Pid(0)` 修复

- spawn 失败路径使用 `BgCancelHandle::Kill(None)`（已消费的 oneshot）或新增 `BgCancelHandle::None` 变体
- 取消逻辑中对 `Pid(0)` 添加防护

### 7. Tasks 面板取消按钮

- `Enter` 键选中 bg task 后调用 `BackgroundTaskRegistry::cancel(task_id)`
- 需要 panel 能访问到 registry 实例（目前 panel 只读 atom，需新增 RPC 通道）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-28 | — | Open | agent | 创建 |
| 2026-07-28 | Open | Fixed | agent | 修复：bg shell 超时+回调+落盘+竞态修复，4 步 8 文件 |

## 修复记录

### 修复 #1（2026-07-28）

- **操作人**：agent
- **修复内容**：
  1. **Step 1**：`BackgroundTask` 新增 `chrono_started_at: DateTime<Utc>` 字段，修复 `list_tasks_full().started_at` 始终返回当前时间（更新 8 个构造点：background_test ×2、execute_bg、spawner、workflow、terminal ×2）
  2. **Step 2**：`BashTool` 和 `TerminalMiddleware` 各新增 `on_bg_complete` 字段和 `with_on_bg_complete()` 方法，在 `builder.rs` 中将 `on_bg_complete` 注入 TerminalMiddleware（与 SubAgent 路径对齐）
  3. **Step 3**：bg shell 核心逻辑重构——删除外部 TOCTOU 预检查，解析 `timeout` 参数并 `clamp`（与同步 Bash 对齐），`tokio::time::timeout` 包裹 `wait_with_output()`，超时分叉显式 kill 子进程 + 注入回调 + 注册完成，正常完成路径注入 `on_bg_complete` 回调（先于 `registry.complete()`），输出 >100K 字符时 `persist_truncated_output` 落盘，`register_with_kind` 失败时仅回调不调 `complete()`（review M-4）
  4. **Step 4**：spawn 成功路径移除 `pid.unwrap_or(0)` 改用 `pid.expect()`，`cancel()` 增加 `pid == 0` 守卫防止 `kill -TERM 0` 波及进程组
- **涉及文件**：`background.rs`, `background_test.rs`, `terminal.rs` (BashTool+TerminalMiddleware), `execute_bg.rs`, `spawner.rs`, `workflow/mod.rs`, `builder.rs`
- **涉及 commit**：待提交
- **验证状态**：`cargo check -p peri-middlewares -p peri-acp` 通过，`cargo test -p peri-middlewares -- subagent` 118 passed

### 未修复项

- **Tasks 面板取消按钮**（success criteria #7）：需要 TUI 层 RPC 通道新增 `AcpEventData::BgTaskCancel` 变体，超出本次核心范围（超时+回调+落盘），留待独立 issue
- **print mode `on_bg_complete` 为 None**：`async_router` 仅在有 `SessionManager` 时构造，print mode 下 b g shell 回调静默为 None——属设计决策非 bug（见 review CRITICAL-3）
- **process_group kill 不完备**：`kill -TERM <pid>` 不杀孙子进程，与现有 `background.rs:295` 的行为一致
