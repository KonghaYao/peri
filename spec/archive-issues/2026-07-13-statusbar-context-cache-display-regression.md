> 归档于 2026-07-18，原路径 spec/issues/2026-07-13-statusbar-context-cache-display-regression.md
# 状态栏上下文消耗显示 + 消息流缓存命中率警告，ratatui-kit 迁移后全部丢失

**状态**：Done
**优先级**：高
**创建日期**：2026-07-13

## 问题描述

ratatui-kit 组件框架迁移（commit `c76b0cfb`）后，两个与 token/context/cache 相关的已有功能全部丢失：

1. **状态栏 Row1 不再显示上下文消耗百分比**——旧实现显示格式为 `CTX 45% 200k`，按阈值染色（<70% 绿 / 70-85% 黄 / >85% 红）
2. **消息流不再注入缓存命中率警告**——旧实现在 `TokenUsageUpdate` 事件中检测 `cache_hit_rate < 80%`，将警告消息注入到聊天消息流中

这两个功能在旧架构中正常运行，迁移到 ratatui-kit 后未被重新接入。

## 症状详情

### 现象 1：状态栏上下文消耗显示缺失

**当前状态栏 Row1 内容**（`status_bar.rs:23-132`）：
```
权限模式 · cwd · provider/model · CPU% · MEM · bg tasks
```

**旧实现内容**（`c76b0cfb^:peri-tui/src/ui/main_ui/status_bar.rs`，render_first_row）：
```rust
// 上下文使用率（放最后）
let tracker = &app.session_mgr.current().agent.session_token_tracker;
if let Some(pct) = tracker.context_usage_percent(/* context_window */) {
    let total = app.session_mgr.current().agent.context_window;
    let color = if pct >= 85.0 { theme::ERROR }
                else if pct >= 70.0 { theme::WARNING }
                else { theme::SAGE };
    // 显示格式: "{:.0}% {}", 如 "45% 200k"
}
```

**当前数据通路**：
- `AgentOutputEvent` → `peri/agent_event`（携带 `budget_pct` + `context_total_tokens`）
- `budget-warning` → `router.rs` → `AcpEventData::BudgetWarning` → `acp_events.rs:354`（仅 `push_acp_state`，无可见通知）
- `usage_update` → `acp_notifier.rs:463-475`（仅写 `SPINNER_TOKEN_COUNT`，未写上下文百分比）

### 现象 2：消息流缓存命中率警告缺失

| 对比维度 | 旧实现（agent_ops.rs） | 新架构 | 状态 |
|----------|----------------------|--------|------|
| 配置开关 | ❌ 无 | `AppConfig.show_cache_warning`（默认 true，config 面板可切换） | ✅ 已有 |
| 显示文案 | 硬编码 | `app-prompt-cache-low`（i18n） | ✅ 已有 |
| **注入逻辑** | `TokenUsageUpdate` handler → `cache_hit_rate() < 0.8` → `PipelineAction::AddMessage` | ❌ 不存在 | **缺失** |

**旧代码**（`ff22951e:agent_ops.rs`，TokenUsageUpdate 分支）：
```rust
let rate = tracker.cache_hit_rate();
if rate < 0.8 {
    let percentage = (rate * 100.0) as u32;
    let req_id = tracker.last_request_id.as_deref().unwrap_or("-");
    let msg = format!("⚠ Prompt cache 命中率 {}% < 80% (req: {})", percentage, req_id);
    // → AddMessage(vm) 注入到消息流
}
```

**当前 `usage_update` 处理器**（`acp_notifier.rs:463-475`）：
```rust
// 只写 SPINNER_TOKEN_COUNT，不计算缓存命中率，不注入警告
*SPINNER_TOKEN_COUNT.state().write() = (input + output) as usize;
// ⬆ 缺少：读取 cacheCreationTokens / cacheReadTokens、计算 hit rate、注入消息
```

## 涉及的已有基础（可复用）

| 组件 | 位置 | 说明 |
|------|------|------|
| `cacheCreationTokens` / `cacheReadTokens` | `usage_update` meta（acp_notifier.rs:463） | 已在 ACP 消息中，当前未读取 |
| `AppConfig.show_cache_warning` | `peri-acp/src/provider/config.rs:156` | 开关已在 config 面板中 |
| `app-prompt-cache-low` i18n | `peri-tui/locales/{en,zh-CN}/main.ftl:291-292` | 文案已就绪，未被引用 |
| `SPINNER_TOKEN_COUNT` atom | `peri-tui/src/kit/atoms.rs:338` | 当前仅存 token 计数，需扩展或新增 atom |
| `TuiSystemNote` 渲染类型 | `acp_events.rs:364-370`（SystemNotification 分支） | 同类型消息注入机制已存在 |

## 修复方向

1. **状态栏上下文消耗**：新增 atom 存储 `(budget_pct, context_total_tokens)`，在 `usage_update` 处理器或 `AgentOutputEvent` 处理器中写入，`StatusBarRow1` 订阅该 atom 并在 Row1 末尾追加 `{:.0}% {total_display}` 显示（带阈值染色）
2. **缓存命中率警告**：在 `usage_update` 处理器（acp_notifier.rs:463）中读取 `cacheCreationTokens` / `cacheReadTokens` meta 字段，计算命中率 `cacheRead / inputTokens`，低于 80% 时构造 `AcpEventData::SystemNotification` 写入 bridge channel

### 现象 3：上下文使用率始终不显示 / 百分比异常（2026-07-13 追加）

修复后仍有两轮相关问题：

**A) 状态栏始终不显示上下文使用率**

状态栏 Row1 第 7 段（CONTEXT_USAGE atom）始终为空。`StateSnapshotMeta` 事件通过 `peri/agent_event` 通道正常到达，`context_total_tokens=Some(200000)` 正确，但 `budget_pct` 始终为 `None`。

**B) 百分比显示为 1293%（而不是 13%）**

修复 budget_pct 读取后，显示值异常偏高。

### 现象 4：BudgetWarning / Compact / AgentExecutionFailed 事件在消息流中不可见（2026-07-13 追加）

以下 ACP 事件都只调用 `push_acp_state`（改 atom 状态，不产生任何用户可见效果）：

| 事件 | 期望行为 | 实际行为 |
|------|----------|----------|
| `BudgetWarning` | 在消息流中显示上下文使用率过高警告 | 无可见提示 |
| `CompactStarted` | 显示"压缩中"提示（或静默） | 无可见提示 |
| `CompactCompleted` | 全量压缩后显示"压缩完成（N 文件，M skills）" | 无可见提示 |
| `CompactError` | 显示压缩失败原因 | 无可见提示 |
| `AgentExecutionFailed` | 显示 Agent 崩溃原因 | 无可见提示 |

## 修复记录

### 修复 #1（2026-07-13）—— TUI 侧实现

- **操作人**：agent
- **用户原意**：恢复状态栏上下文使用率显示 + 缓存命中率警告 + 消息流事件注入
- **修复内容**：
  - `atoms.rs`：新增 `CONTEXT_USAGE` / `CACHE_HIT_INFO` 两个 atom
  - `acp_notifier.rs`：`StateSnapshotMeta` 处理器从 `budget_pct` 写 `CONTEXT_USAGE`；`usage_update` 处理器读 `cacheReadTokens` 写 `CACHE_HIT_INFO`
  - `acp_events.rs`：`inject_cache_warning()` 在 TurnDone/TurnSuspended 时检查 `CACHE_HIT_INFO` 并注入消息流（<80% 阈值）；`BudgetWarning` / `CompactCompleted`（全量）/ `CompactError` / `AgentExecutionFailed` 均注入 `TuiSystemNote` 到 committed
  - `status_bar.rs`：`StatusBarRow1` 订阅 `CONTEXT_USAGE`，Row1 末尾追加 `{:.0}% {total_display}` 显示
  - 清理冗余：删除 `CONTEXT_WINDOW` atom、诊断日志（`ctx_usage_diag` / `cache_hit_diag`）、usage_update 后备计算
- **涉及 commit**：`2d05b928`
- **验证状态**：部分验证（上下文使用率仍不显示——见修复 #2 和 #3）

### 修复 #2（2026-07-13）—— agent 侧 `budget_pct` 始终为 None

- **操作人**：agent
- **用户原意**：上下文使用率应正常显示
- **修复内容**：`agent_context.rs:78` — `from_stage()` 原本每次创建全新 `TokenTracker::default()`（空实例），导致 `act.rs` 读取 `budget_pct` 时永远无 `last_usage`。改为 `ctx.token_tracker.read().clone()` 从共享 `StageContext.token_tracker` 读取。
- **涉及 commit**：待提交
- **验证状态**：待验证

### 修复 #3（2026-07-13）—— 百分比显示 1293%

- **操作人**：agent
- **用户原意**：百分比应在 0-100 范围内
- **修复内容**：`acp_notifier.rs` — agent 侧 `context_usage_percent()` 已返回 0-100 百分比值，TUI 端不再乘以 100。
- **涉及 commit**：待提交
- **验证状态**：待验证

### 修复 #4（2026-07-13）—— 界面非每次 LLM 调用都更新

- **操作人**：agent
- **用户原意**：每次 LLM 调用后状态栏应立即反映最新使用率
- **修复内容**：`acp_notifier.rs` — 写入 `CONTEXT_USAGE` 后递增 `RENDER_HEARTBEAT` 确保渲染帧唤醒。
- **涉及 commit**：待提交
- **验证状态**：待验证

## 涉及文件

- `peri-agent/src/agent/agent_context.rs` —— `from_stage()` 从共享 token_tracker clone 而非创建空实例
- `peri-tui/src/kit/status_bar.rs` —— StatusBarRow1 追加上下文消耗显示
- `peri-tui/src/kit/atoms.rs` —— 新增 CONTEXT_USAGE / CACHE_HIT_INFO atom
- `peri-tui/src/kit/acp_notifier.rs` —— StateSnapshotMeta 写入 CONTEXT_USAGE；usage_update 写入 CACHE_HIT_INFO；RENDER_HEARTBEAT 递增
- `peri-tui/src/kit/acp_events.rs` —— inject_cache_warning 函数；BudgetWarning/Compact*/AgentExecutionFailed 分支
- `peri-tui/src/kit/acp_bridge.rs` —— BudgetWarning/Compact* 日志字符串（次要）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | Open | Partial | agent | TUI 侧修复完成（#1），agent 侧仍有 bug（budget_pct 始终 None / 双重*100 / 界面更新不及时）

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
