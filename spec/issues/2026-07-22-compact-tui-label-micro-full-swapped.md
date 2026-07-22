# Micro/Full Compact TUI 通知标签互换 bug

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-22

## 问题描述

Compact 完成后 TUI 消息流中注入的 `TuiSystemNote` 显示的 compact 类型（"微压缩"/"完整压缩"）与实际执行的 compact 类型**互换**了。Micro Compact 完成后用户看到的是"完整压缩完成"，Full Compact 完成后用户看到的是"微压缩完成"。

## 症状详情

### 根因

`events_v2_mapper.rs:182` 中 `micro_cleared` 的计算和 `acp_events.rs:818` 中的判定逻辑是反的。

**mapper 中**：
```rust
micro_cleared: before_count.saturating_sub(after_count),
```

| Compact 类型 | before | after | micro_cleared | 
|-------------|--------|-------|:---:|
| Micro（truncated，消息仍 visible） | 20 | 20 | **0** |
| Full（excluded，消息不可见） | 20 | 3 | **17** |

**TUI 中**：
```rust
// acp_events.rs:818
let compact_type = if *micro_cleared > 0 {
    i18n::tr("app-note-compact-type-micro")   // "微压缩"
} else {
    i18n::tr("app-note-compact-type-full")    // "完整压缩"
};
```

| 实际执行的 Compact | micro_cleared | TUI 显示 | 正确？ |
|-------------------|:---:|---------|:---:|
| Micro | 0 | **"完整压缩完成"** | ❌ |
| Full | 17 | **"微压缩完成"** | ❌ |

### 为什么以前没发现

`CompactCompleted` 事件已有 `strategy: CompactStrategy` 字段（`Micro`/`Full`），但 TUI handler 没有使用它，而是错误地用 `micro_cleared` 推导类型。

## 涉及文件

- `peri-tui/src/kit/acp_events.rs:818` —— TUI handler：用 `micro_cleared > 0` 判断类型（bug 位置）
- `peri-agent/src/agent/events_v2_mapper.rs:182` —— mapper：计算 `micro_cleared = before - after`（无 bug，但命名误导）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建——排查 Micro Compact 通知路径时发现 |
| 2026-07-22 | Open | Fixed | agent | 修复：传播 strategy 字段至 AcpEventData，TUI 用 strategy 替代 micro_cleared 判断类型 |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：修复 Micro/Full Compact TUI 通知标签互换——Micro 显示"微压缩"，Full 显示"完整压缩"
- **修复内容**：
  1. `acp_types.rs`：`CompactCompleted` 新增 `strategy: String` 字段
  2. `v2_bridge.rs`：从 `ObserveEvent::MessagesCompacted` 提取 strategy 并传递
  3. `event/mod.rs`：`AcpEvent::CompactCompleted` 新增 `strategy: String`
  4. `event_sink.rs`：新增 `ExecutorEvent→AcpEvent` 的 strategy 映射
  5. `acp_notifier.rs`：传递 strategy 从 AcpEvent 到 AcpEventData
  6. `acp_events.rs`：用 `strategy` 字段替代 `micro_cleared` 判断 compact 类型标签
- **验证状态**：已验证
