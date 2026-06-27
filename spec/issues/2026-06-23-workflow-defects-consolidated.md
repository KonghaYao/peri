# workflow 系统缺陷合集：通知 / 统计 / 面板 / 可执行权限 / 文档

**状态**：Fixed
**优先级**：高
**类型**：Bug / Doc
**创建日期**：2026-06-23
**关联**：`feature/workflow-ultracode` squash `b22196ab`

> 以下 11 个缺陷均在 workflow 系统开发过程中发现并修复。合并为单 issue 归档。

---

## 缺陷索引

### A. 通知管线（3 项）

| ID | 标题 | 根因 | 修复方式 |
|----|------|------|----------|
| A1 | 完成通知未可靠返回 | per-turn forwarder 在 agent 结束后销毁，后续 workflow 完成通知丢失 | 新增 session 级 forwarder（永久运行），PULL 模型 drain |
| A2 | 完成通知重复刷屏（13+ 次） | 每个 `build_agent()` 创建一个 forwarder receiver，从不清理 | `WorkflowMiddleware.swap_forwarder_abort()` abort 旧 handle |
| A3 | agent 不自动响应 workflow 完成 | PULL 模型仅在 `execute_prompt()` 时 drain，无 prompt 时不触发 | TUI `handle_background_task_completed` 检测 `workflow:` 前缀，推 `pending_messages` 自动 flush |

### B. 统计字段（1 项）

| ID | 标题 | 根因 | 修复方式 |
|----|------|------|----------|
| B1 | 通知显示 "0 agents / 0 calls" | 多轮修复：(1) `AgentRunResult` 硬编码 0 → 从 LLM 事件累积；(2) `agent_count` 从 progress_store 读取（修时序）；(3) `tool_calls_count` 从 `AgentRunResult.tool_count` 提取 | `workflow_agent.rs` LLM 事件累积器 + `progress.rs` AgentDone handler + `tool.rs` fallback |

### C. 面板（1 项）

| ID | 标题 | 根因 | 修复方式 |
|----|------|------|----------|
| C1 | workflow 面板全程空白/不更新 | 面板未监听 `WorkflowTaskRegistry` 的 progress 事件流 | `WorkflowPanel` 订阅 registry 事件，`poll_agent → handle_acp_notification → AcpNotification::AgentEvent` 更新 panel state |

### D. 运行时错误（2 项）

| ID | 标题 | 根因 | 修复方式 |
|----|------|------|----------|
| D1 | workflow 失败但无错误可追溯 | `workflow/done` RPC 通知漏发 `error` 字段；`state.json` 未持久化 error | `runner.js` 补发 error → `WorkflowDoneParams.error`；`registry.complete()` 写入 RunState.error |
| D2 | sub-agent 编辑后 runner.js 丢失可执行权限 | Write 工具不保留文件的可执行位 | ~~修复~~ 记录为已知限制，建议用 `chmod` 恢复 |

### E. 文档 / Skill（3 项）

| ID | 标题 | 修复方式 |
|----|------|----------|
| E1 | pipeline/phase/workflow 签名与实际不一致 | 修正 `SKILL.md` 示例签名，对齐 engine hooks.js |
| E2 | parallel() 示例缺少工厂函数 | 修正示例：`agent("...")` → `() => agent("...")` |
| E3 | pipeline() 空数组行为未文档化 | 已在 SKILL.md 补充文档说明 |

### F. 时间戳（1 项）

| ID | 标题 | 修复方式 |
|----|------|----------|
| F1 | state.json 时间戳不反映真实运行时间 | `runner.js` 改用 `Date.now()` 记录真实 started_at/finished_at（微秒精度从 ISO 8601 解析） |

---

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-23 | — | Fixed | agent | 合并 11 个已修复 workflow 缺陷为单 issue 归档 |

## 修复记录

全部缺陷均在 `feature/workflow-ultracode` 分支的 squash commit `b22196ab`（113 files, +16015/-487）中修复。详细修复步骤见各原始 issue 文件。

### 已删除的原始 issue 文件

以下 11 个文件的内容已合并到此文档：

- `2026-06-22-workflow-fails-runner-not-found-and-error-swallowed.md`
- `2026-06-23-file-edit-strips-executable-permission.md`
- `2026-06-23-pipeline-empty-error-behavior-undocumented.md`
- `2026-06-23-ultracode-skill-parallel-example-wrong-signature.md`
- `2026-06-23-workflow-capability-check-exposes-doc-and-notification-defects.md`
- `2026-06-23-workflow-completion-notification-missing.md`
- `2026-06-23-workflow-completion-notification-repeated-flood.md`
- `2026-06-23-workflow-completion-stats-zero-but-agents-ran.md`
- `2026-06-23-workflow-completion-system-reminder-no-agent-reaction.md`
- `2026-06-23-workflow-panel-no-realtime-update.md`
- `2026-06-23-workflow-state-timestamps-microsecond-resolution.md`
