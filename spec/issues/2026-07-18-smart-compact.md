# Smart Compact 实现——LLM 决策式消息保留/丢弃策略

**状态**：Open
**优先级**：低
**创建日期**：2026-07-18
**父 issue**：`spec/issues/residual-code-scan-20260718.md` (P0-2, P1-5)

## 背景

目前 compact_v2 有两条策略：

| 策略 | 文件 | 实现状态 | 触发条件 |
|------|------|:--:|------|
| Full Compact | `full.rs` | ✅ 完 | usage > 100% |
| Micro Compact | `micro.rs` | ✅ 完成 | usage > 70%, `< 85%` |
| Smart Compact | `smart.rs` | ❌ 9 行空壳 | usage > 85%, `< 100%` |

当上下文使用率 > 85% 时，Smart Compact 应触发，但实际在 `mod.rs:run_compact` 中降级为 Micro Compact。

## 设计意图（来自 smart.rs 注释）

> 使用 LLM 决策保留消息 id + 未选中标 `excluded` + 追加 system-reminder

核心思路：不再无条件丢弃旧消息，而是让 LLM 分析对话并决定哪些消息关键、哪些可安全排除。

## 影响范围

| 文件 | 角色 |
|------|------|
| `peri-agent/src/agent/compact_v2/smart.rs` | 主要实现文件 |
| `peri-agent/src/agent/compact_v2/mod.rs` | `run_compact` 降级逻辑移除 |
| `peri-agent/src/agent/compact_v2/event_builder.rs` | CompactStarted/MessagesCompacted 已支持 `Smart` 策略值 |
| `peri-agent/src/agent/compact_v2/full.rs` | 可能复用 `extract_summary_into_prefix` |

## 基本实现步骤

1. 在 `smart.rs` 中实现 `run_smart_compact(context, messages, budget)` 函数
2. 输入：当前消息历史 + 上下文预算
3. 调用 LLM（小模型 / cheap model）分析消息，输出保留/丢弃决策
4. 执行决策：修剪消息 + 追加 system-reminder
5. 发送 CompactStarted/MessagesCompacted 事件（strategy = Smart）
6. 在 `mod.rs::run_compact` 中移除降级逻辑，切换到 Smart 分支

## 验证标准

- [ ] 触发 Smart Compact 后消息数减少
- [ ] LLM 决策的消息保留/丢弃合理
- [ ] system-reminder 包含"已精简 N 条消息"说明
- [ ] `cargo test -p peri-agent --lib` 全过
- [ ] budget pct 回到 50-70% 区间
