# Micro Compact 执行后 TUI 不显示 SystemNote

> **状态**：Open | **优先级**：中 | **类型**：Bug | **日期**：2026-07-29

## 问题描述

Auto compact 触发 Micro Compact 且成功执行后（truncated 标记已应用到 transcript），TUI 界面上完全看不到 compact 完成的通知（SystemNote）。用户无法感知到 compact 发生。

## 症状详情

- **复现频率**：必现
- **触发条件**：对话过程中 budget ≥ 75% 自动触发 Micro Compact
- **表现**：Micro Compact 确实执行了（截断效果生效），但消息流中没有任何 SystemNote（如 "微压缩完成" 提示条）
- **期望**：Micro Compact 执行后应显示 "微压缩完成" 之类的 SystemNote

## 根因分析

经全链路（`run_compact` → `MessagesCompacted` → `ExecutorEvent::CompactCompleted` → `AcpEvent::CompactCompleted` → `handle_compact_completed` → `inject_system_note`）代码审查，事件流向完整无断裂。问题出在 **Debug 格式 vs 预期文字面值的错配**。

### 核心问题：`CompactOutcome` 序列化用了 Debug 而非 serde snake_case

**文件**：`peri-acp/src/session/event_sink.rs:207-208`

```rust
let strategy_str = format!("{:?}", strategy).to_lowercase();
let outcome_str = format!("{:?}", outcome).to_lowercase();
```

`CompactOutcome` 和 `CompactStrategy` 都 derive 了 `Debug`，但没有 derive `Display`。这里的 `format!("{:?}", ...)` 产生的是 Rust Debug 格式：

| CompactOutcome 变体 | Debug 输出 | `.to_lowercase()` | TUI 期望匹配 |
|---------------------|-----------|-------------------|-------------|
| `MicroApplied` | `MicroApplied` | `microapplied` | `micro_applied` |
| `FullApplied` | `FullApplied` | `fullapplied` | `full_applied` |
| `SmartApplied` | `SmartApplied` | `smartapplied` | `smart_applied` |
| `FullFailed` | `FullFailed` | `fullfailed` | `full_failed` |
| `MicroAppliedThenFullFailed` | `MicroAppliedThenFullFailed` | `microappliedthenfullfailed` | `micro_applied_then_full_failed` |
| `SmartAppliedThenFullFailed` | `SmartAppliedThenFullFailed` | `smartappliedthenfullfailed` | `smart_applied_then_full_failed` |
| `InterruptedAfterCommit` | `InterruptedAfterCommit` | `interruptedaftercommit` | `interrupted_after_commit` |
| `Skipped` | `Skipped` | `skipped` | — |
| `Shadowed` | `Shadowed` | `shadowed` | `shadowed` ✅ |

**只有 `Shadowed` 能匹配**（刚好 Debug 格式 = snake_case），其他所有变体均错配。

### TUI 端的匹配逻辑

**文件**：`peri-tui/src/kit/acp_events/compact.rs:34-49`

```rust
let compact_type = match outcome {
    "micro_applied" => ...,
    "smart_applied" => ...,
    "full_applied" => ...,
    "full_failed" => return,            // 静默跳过
    "shadowed" => return,               // 静默跳过
    "micro_applied_then_full_failed" => return,  // 静默跳过
    "smart_applied_then_full_failed" => return,  // 静默跳过
    "interrupted_after_commit" => return,        // 静默跳过
    _ => {
        // fallback: 按 strategy 判断
        match strategy {
            "micro" => i18n::tr("app-note-compact-type-micro"),
            _ => i18n::tr("app-note-compact-type-full"),
        }
    }
};
```

因为 Debug 格式与 TUI 期望不匹配，**所有 outcome 都落入 `_` fallback 分支**。对于 Micro Compact，fallback 通过 `strategy = "micro"` 匹配成功，理论上应能显示 SystemNote。

### 为什么用户看不到 SystemNote？

排除事件链路断裂后，最可能的原因是 **`strategy` 字段的 Debug 输出在特定条件下也不匹配**。`CompactStrategy` 的 Debug 格式：

| CompactStrategy | Debug | `.to_lowercase()` | TUI 匹配 |
|-----------------|-------|-------------------|---------|
| `Micro` | `Micro` | `micro` | `"micro"` ✅ |
| `Full` | `Full` | `full` | `_` → `"full"` ✅ |
| `Smart` | `Smart` | `smart` | `_` → `"full"` ⚠️ |
| `Skip` | `Skip` | `skip` | `_` → `"full"` ⚠️ |

对于 Micro Compact，`strategy = "micro"` 匹配成功，SystemNote **应能注入**。

但 `CompactStrategy::Skip` 和 `CompactStrategy::Smart` 会错误地显示为 "Full Compact"。

要进一步确认为何 SystemNote 完全不显示，需要在运行时加日志追踪 `handle_compact_completed` 的 `strategy` / `outcome` 入参值。

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-acp/src/session/event_sink.rs:207-208` | 用 Debug 格式序列化 outcome/strategy |
| `peri-tui/src/kit/acp_events/compact.rs:34-49` | TUI handler 期望 snake_case 匹配 |
| `peri-agent/src/agent/compact_v2/mod.rs` | `CompactOutcome` / Compact 执行结果 |
| `peri-agent/src/agent/events.rs` | `CompactStrategy` 定义 |
| `peri-agent/src/agent/stages/compact.rs` | `MessagesCompacted` 事件发射 |

## 修复方向

### 方案 A（推荐）：改用 serde 序列化

`CompactOutcome` 和 `CompactStrategy` 已 derive `serde::Serialize` 且有 `#[serde(rename_all = "snake_case")]`。应将 `event_sink.rs` 中的：

```rust
let outcome_str = format!("{:?}", outcome).to_lowercase();
let strategy_str = format!("{:?}", strategy).to_lowercase();
```

改为使用 serde 的 snake_case 序列化。最简单的方式是给这两个枚举 derive/impl `Display`，或者直接用 `serde_json::to_string(&outcome)` 再 trim quotes。

### 方案 B：给 CompactOutcome/CompactStrategy 实现 Display

```rust
impl std::fmt::Display for CompactOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_string(self).unwrap_or_default();
        write!(f, "{}", s.trim_matches('"'))
    }
}
```

## 验证标准

修复后：
1. Micro Compact 执行后 TUI 应显示 "微压缩完成"
2. Full Compact 执行后 TUI 应显示 "完整压缩完成"
3. `MicroAppliedThenFullFailed` / `FullFailed` / `InterruptedAfterCommit` 等 outcome 不应显示 SystemNote（静默跳过）
4. `Shadowed` outcome 不应显示 SystemNote

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-29 | — | Open | agent | 创建，根因定位到 Debug/snake_case 格式错配 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
